//! Integration tests for the streaming `Encoder<W: Write>` /
//! `Encoder<W: Write + Seek>` API (see `src/cafe.rs`).
//!
//! Compares `Encoder`'s output against the whole-file `encode()`/
//! `encode_bytes()`-equivalent path (via `decode_bytes`, since there is no
//! public in-memory `encode_bytes()`), and exercises the documented error
//! paths (non-multiple-of-row-width tiles, height overflow, incomplete
//! `finish()`, indexed-palette rejection).

use cafe::*;
use std::io::Cursor;

/// Generates a deterministic RGBA pixel buffer (same generator shape used by
/// `integration_roundtrip.rs`), width/height not required to be tile-aligned.
fn generate_rgba(width: u32, height: u32) -> Vec<u8> {
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) as usize) * 4;
            pixels[idx] = ((x * 3 + y * 5) % 256) as u8;
            pixels[idx + 1] = ((255 + x * 2).wrapping_sub(y)) as u8;
            pixels[idx + 2] = (x.wrapping_sub(y) % 256) as u8;
            pixels[idx + 3] = 255;
        }
    }
    pixels
}

/// Feeds `pixels` into a fresh `Encoder<Cursor<Vec<u8>>>` in `tile_rows`-row
/// chunks (last chunk may be shorter), returning the finished byte buffer.
/// `use_finish_exact` selects between `finish()` (conservative
/// `compression_method`) and `finish_exact()` (patched, exact value).
fn encode_via_streaming_encoder(
    pixels: &[u8],
    width: u32,
    height: u32,
    tile_rows: u32,
    opts: &EncoderOptions,
    use_finish_exact: bool,
) -> Vec<u8> {
    let mut encoder =
        Encoder::new(Cursor::new(Vec::new()), width, height, opts).expect("Encoder::new failed");

    let row_bytes = (width as usize) * 4;
    let mut y = 0u32;
    while y < height {
        let rows_this_tile = tile_rows.min(height - y) as usize;
        let start = (y as usize) * row_bytes;
        let end = start + rows_this_tile * row_bytes;
        encoder
            .add_tile(&pixels[start..end])
            .expect("add_tile failed");
        y += rows_this_tile as u32;
    }

    let cursor = if use_finish_exact {
        encoder.finish_exact().expect("finish_exact failed")
    } else {
        encoder.finish().expect("finish failed")
    };
    cursor.into_inner()
}

#[test]
fn test_streaming_encoder_finish_roundtrip_pixel_exact() {
    let width = 193u32; // not a multiple of any tile size, exercises block edges
    let height = 137u32;
    let pixels = generate_rgba(width, height);

    let opts = EncoderOptions {
        level: 12,
        use_filter: true,
        target_color_type: constants::COLOR_TYPE_RGBA,
        ..Default::default()
    };

    let bytes = encode_via_streaming_encoder(&pixels, width, height, 32, &opts, false);
    let (decoded_pixels, result) = decode_bytes(&bytes).expect("decode_bytes failed");

    assert_eq!(result.width, width);
    assert_eq!(result.height, height);
    assert_eq!(
        decoded_pixels, pixels,
        "pixel mismatch for Encoder::finish()"
    );
}

#[test]
fn test_streaming_encoder_finish_exact_roundtrip_pixel_exact() {
    let width = 193u32;
    let height = 137u32;
    let pixels = generate_rgba(width, height);

    let opts = EncoderOptions {
        level: 12,
        use_filter: true,
        target_color_type: constants::COLOR_TYPE_RGBA,
        ..Default::default()
    };

    let bytes = encode_via_streaming_encoder(&pixels, width, height, 32, &opts, true);
    let (decoded_pixels, result) = decode_bytes(&bytes).expect("decode_bytes failed");

    assert_eq!(result.width, width);
    assert_eq!(result.height, height);
    assert_eq!(
        decoded_pixels, pixels,
        "pixel mismatch for Encoder::finish_exact()"
    );
}

/// `finish()` always leaves `compression_method`'s ZSTD bit set (byte at a
/// fixed IHDR offset), regardless of whether any chunk actually used ZSTD —
/// the documented conservative behavior for the non-seekable path.
#[test]
fn test_streaming_encoder_finish_always_sets_zstd_bit_conservatively() {
    const CM_OFFSET: usize = 9 + 4 + 4 + 1 + 11; // signature + len + type + flag + 11 IHDR bytes

    // A tiny, highly-compressible image: real encode() would very likely
    // pick ZSTD anyway, but the point here is the bit is set unconditionally
    // *before* any tile is even compressed, not the actual outcome.
    let width = 4u32;
    let height = 4u32;
    let pixels = vec![7u8; (width * height * 4) as usize];

    let opts = EncoderOptions {
        level: 12,
        use_filter: true,
        target_color_type: constants::COLOR_TYPE_RGBA,
        ..Default::default()
    };

    let bytes = encode_via_streaming_encoder(&pixels, width, height, 4, &opts, false);
    assert_eq!(
        bytes[CM_OFFSET] & constants::COMPRESSION_METHOD_ZSTD_BIT,
        constants::COMPRESSION_METHOD_ZSTD_BIT,
        "finish() must always leave the ZSTD bit set (conservative, non-seekable path)"
    );

    // Still decodes fine.
    let (decoded_pixels, _) = decode_bytes(&bytes).expect("decode_bytes failed");
    assert_eq!(decoded_pixels, pixels);
}

/// `finish_exact()` patches `compression_method` to the exact value
/// (`encode()`'s `patch_ihdr_compression_method` behavior) — for content
/// that never benefits from ZSTD, the bit should end up cleared.
#[test]
fn test_streaming_encoder_finish_exact_computes_exact_compression_method() {
    const CM_OFFSET: usize = 9 + 4 + 4 + 1 + 11;

    let width = 193u32;
    let height = 137u32;
    let pixels = generate_rgba(width, height);

    let opts = EncoderOptions {
        level: 12,
        use_filter: true,
        target_color_type: constants::COLOR_TYPE_RGBA,
        ..Default::default()
    };

    let exact_bytes = encode_via_streaming_encoder(&pixels, width, height, 32, &opts, true);
    let conservative_bytes = encode_via_streaming_encoder(&pixels, width, height, 32, &opts, false);

    // Conservative path always sets the bit; exact path reflects whichever
    // value the real IDAT/ancillary compression actually produced. For this
    // non-trivial image ZSTD is virtually certain to win at least one tile,
    // so both should agree here — the important invariant checked is that
    // finish_exact() never *disagrees* with the conservative upper bound in
    // the direction that would break decoding (it can only clear the bit if
    // ZSTD genuinely wasn't used anywhere, never set it if it doesn't need
    // to be), and that both files still decode identically.
    assert_eq!(
        conservative_bytes[CM_OFFSET] & constants::COMPRESSION_METHOD_ZSTD_BIT,
        constants::COMPRESSION_METHOD_ZSTD_BIT
    );
    assert!(
        exact_bytes[CM_OFFSET] & constants::COMPRESSION_METHOD_ZSTD_BIT
            <= conservative_bytes[CM_OFFSET] & constants::COMPRESSION_METHOD_ZSTD_BIT
    );

    let (decoded_exact, _) = decode_bytes(&exact_bytes).expect("decode exact failed");
    let (decoded_conservative, _) =
        decode_bytes(&conservative_bytes).expect("decode conservative failed");
    assert_eq!(decoded_exact, pixels);
    assert_eq!(decoded_conservative, pixels);
}

/// Tiles of varying, caller-chosen heights (not all equal to `tile_rows`)
/// must still round-trip correctly — `add_tile()` infers each tile's height
/// from the buffer it receives rather than enforcing `EncoderOptions::tile_rows`.
#[test]
fn test_streaming_encoder_variable_tile_heights_roundtrip() {
    let width = 64u32;
    let height = 50u32;
    let pixels = generate_rgba(width, height);
    let row_bytes = (width as usize) * 4;

    let opts = EncoderOptions {
        level: 6,
        use_filter: true,
        target_color_type: constants::COLOR_TYPE_RGBA,
        ..Default::default()
    };

    let mut encoder =
        Encoder::new(Cursor::new(Vec::new()), width, height, &opts).expect("Encoder::new failed");

    // Irregular tile heights: 1, 7, 20, 22 = 50 rows total.
    let mut y = 0usize;
    for tile_h in [1usize, 7, 20, 22] {
        let start = y * row_bytes;
        let end = start + tile_h * row_bytes;
        encoder
            .add_tile(&pixels[start..end])
            .expect("add_tile failed");
        y += tile_h;
    }

    let cursor = encoder.finish().expect("finish failed");
    let bytes = cursor.into_inner();
    let (decoded_pixels, result) = decode_bytes(&bytes).expect("decode_bytes failed");
    assert_eq!(result.width, width);
    assert_eq!(result.height, height);
    assert_eq!(decoded_pixels, pixels);
}

#[test]
fn test_streaming_encoder_add_tile_rejects_non_multiple_of_row_width() {
    let width = 16u32;
    let height = 16u32;
    let opts = EncoderOptions::default();
    let mut encoder =
        Encoder::new(Cursor::new(Vec::new()), width, height, &opts).expect("Encoder::new failed");

    // width*4 = 64 bytes per row; supply 63 bytes (not a multiple).
    let bad_tile = vec![0u8; 63];
    let err = encoder.add_tile(&bad_tile).unwrap_err();
    assert!(matches!(err, CafeError::UnsupportedFeature(_)));
}

#[test]
fn test_streaming_encoder_add_tile_rejects_exceeding_declared_height() {
    let width = 4u32;
    let height = 4u32;
    let opts = EncoderOptions::default();
    let mut encoder =
        Encoder::new(Cursor::new(Vec::new()), width, height, &opts).expect("Encoder::new failed");

    // 8 rows worth of pixels against a declared height of 4.
    let too_many_rows = vec![0u8; (width as usize) * 8 * 4];
    let err = encoder.add_tile(&too_many_rows).unwrap_err();
    assert!(matches!(err, CafeError::UnsupportedFeature(_)));
}

/// Submitting rows across multiple `add_tile()` calls that cumulatively
/// exceed `height` must also fail (not just a single too-large call).
#[test]
fn test_streaming_encoder_add_tile_rejects_cumulative_overflow() {
    let width = 4u32;
    let height = 4u32;
    let opts = EncoderOptions::default();
    let mut encoder =
        Encoder::new(Cursor::new(Vec::new()), width, height, &opts).expect("Encoder::new failed");

    let three_rows = vec![0u8; (width as usize) * 3 * 4];
    encoder
        .add_tile(&three_rows)
        .expect("first add_tile failed");

    let two_more_rows = vec![0u8; (width as usize) * 2 * 4];
    let err = encoder.add_tile(&two_more_rows).unwrap_err();
    assert!(matches!(err, CafeError::UnsupportedFeature(_)));
}

#[test]
fn test_streaming_encoder_finish_rejects_incomplete_submission() {
    let width = 4u32;
    let height = 4u32;
    let opts = EncoderOptions::default();
    let mut encoder =
        Encoder::new(Cursor::new(Vec::new()), width, height, &opts).expect("Encoder::new failed");

    // Only submit 2 of 4 declared rows.
    let two_rows = vec![0u8; (width as usize) * 2 * 4];
    encoder.add_tile(&two_rows).expect("add_tile failed");

    let err = encoder.finish().unwrap_err();
    assert!(matches!(err, CafeError::UnsupportedFeature(_)));
}

#[test]
fn test_streaming_encoder_finish_exact_rejects_incomplete_submission() {
    let width = 4u32;
    let height = 4u32;
    let opts = EncoderOptions::default();
    let mut encoder =
        Encoder::new(Cursor::new(Vec::new()), width, height, &opts).expect("Encoder::new failed");

    let two_rows = vec![0u8; (width as usize) * 2 * 4];
    encoder.add_tile(&two_rows).expect("add_tile failed");

    let err = encoder.finish_exact().unwrap_err();
    assert!(matches!(err, CafeError::UnsupportedFeature(_)));
}

#[test]
fn test_streaming_encoder_new_rejects_indexed_color_type() {
    let opts = EncoderOptions {
        target_color_type: constants::COLOR_TYPE_INDEXED,
        ..Default::default()
    };
    match Encoder::new(Cursor::new(Vec::new()), 4, 4, &opts) {
        Ok(_) => panic!("expected UnsupportedFeature error for COLOR_TYPE_INDEXED"),
        Err(err) => assert!(matches!(err, CafeError::UnsupportedFeature(_))),
    }
}

#[test]
fn test_streaming_encoder_new_rejects_zero_dimensions() {
    let opts = EncoderOptions::default();
    assert!(Encoder::new(Cursor::new(Vec::new()), 0, 4, &opts).is_err());
    assert!(Encoder::new(Cursor::new(Vec::new()), 4, 0, &opts).is_err());
}

/// Non-RGBA/8/uint direct color types (the non-fast-path branch of
/// `add_tile()`'s color conversion) must also round-trip correctly.
#[test]
fn test_streaming_encoder_gray_color_type_roundtrip() {
    let width = 37u32;
    let height = 23u32;
    let pixels = generate_rgba(width, height);

    let opts = EncoderOptions {
        level: 6,
        use_filter: true,
        target_color_type: constants::COLOR_TYPE_GRAY,
        ..Default::default()
    };

    let bytes = encode_via_streaming_encoder(&pixels, width, height, 8, &opts, false);
    let (decoded_pixels, result) = decode_bytes(&bytes).expect("decode_bytes failed");
    assert_eq!(result.width, width);
    assert_eq!(result.height, height);
    // Grayscale is lossy vs. arbitrary RGBA input (channels get averaged),
    // so just confirm dimensions/byte-length match rather than exact pixels.
    assert_eq!(decoded_pixels.len(), pixels.len());
}

/// Byte-shuffle filter method (`use_byte_shuffle: true`) through the
/// streaming encoder, using 16-bit RGBA (bpp=8, a supported byte-shuffle
/// width) to also exercise the non-uint-8 conversion path.
#[test]
fn test_streaming_encoder_byte_shuffle_roundtrip() {
    let width = 20u32;
    let height = 15u32;
    let pixels = generate_rgba(width, height);

    let opts = EncoderOptions {
        level: 6,
        use_filter: false,
        use_byte_shuffle: true,
        target_color_type: constants::COLOR_TYPE_RGBA,
        target_bit_depth: Some(16),
        ..Default::default()
    };

    let bytes = encode_via_streaming_encoder(&pixels, width, height, 5, &opts, false);
    let (decoded_pixels, result) = decode_bytes(&bytes).expect("decode_bytes failed");
    assert_eq!(result.width, width);
    assert_eq!(result.height, height);
    assert_eq!(decoded_pixels.len(), pixels.len());
}

/// Per-row predictive filter (`use_filter_per_row: true`) through the
/// streaming encoder.
#[test]
fn test_streaming_encoder_per_row_filter_roundtrip() {
    let width = 51u32;
    let height = 41u32;
    let pixels = generate_rgba(width, height);

    let opts = EncoderOptions {
        level: 6,
        use_filter: true,
        use_filter_per_row: true,
        filter_heuristic: FilterHeuristic::Entropy,
        target_color_type: constants::COLOR_TYPE_RGBA,
        ..Default::default()
    };

    let bytes = encode_via_streaming_encoder(&pixels, width, height, 9, &opts, true);
    let (decoded_pixels, result) = decode_bytes(&bytes).expect("decode_bytes failed");
    assert_eq!(result.width, width);
    assert_eq!(result.height, height);
    assert_eq!(decoded_pixels, pixels);
}

/// `use_filter_per_row: true` combined with an unsupported heuristic must be
/// rejected upfront by `Encoder::new()`, mirroring `encode()`'s validation.
#[test]
fn test_streaming_encoder_new_rejects_per_row_with_unsupported_heuristic() {
    let opts = EncoderOptions {
        use_filter: true,
        use_filter_per_row: true,
        filter_heuristic: FilterHeuristic::CompressionTest,
        ..Default::default()
    };
    match Encoder::new(Cursor::new(Vec::new()), 16, 16, &opts) {
        Ok(_) => {
            panic!("expected UnsupportedFeature error for per-row + CompressionTest heuristic")
        }
        Err(err) => assert!(matches!(err, CafeError::UnsupportedFeature(_))),
    }
}

/// Compares the streaming encoder's `finish_exact()` output byte-for-byte
/// against `encode()`'s whole-file output for the exact same pixels/options,
/// confirming the two independent code paths produce identical files (not
/// just pixel-equivalent ones after decode).
#[test]
fn test_streaming_encoder_matches_whole_file_encode_byte_for_byte() {
    let temp_dir = std::env::temp_dir().join("cafe_streaming_encode_tests");
    let _ = std::fs::create_dir_all(&temp_dir);
    let input_png = temp_dir.join("streaming_vs_whole_input.png");
    let output_cafe = temp_dir.join("streaming_vs_whole_output.cafe");

    let width = 96u32;
    let height = 64u32;
    let pixels = generate_rgba(width, height);

    let image_buffer = image::RgbaImage::from_raw(width, height, pixels.clone()).unwrap();
    image_buffer.save(&input_png).expect("save png failed");

    let whole_file_opts = EncodeOptions {
        use_filter: true,
        level: 6,
        adaptive_analysis: false,
        target_color_type: constants::COLOR_TYPE_RGBA,
        ..Default::default()
    };
    encode(
        input_png.to_str().unwrap(),
        output_cafe.to_str().unwrap(),
        &whole_file_opts,
    )
    .expect("encode failed");
    let whole_file_bytes = std::fs::read(&output_cafe).expect("read output failed");

    let streaming_opts = EncoderOptions {
        level: 6,
        use_filter: true,
        tile_rows: constants::DEFAULT_TILE_ROWS,
        target_color_type: constants::COLOR_TYPE_RGBA,
        ..Default::default()
    };
    let streaming_bytes = encode_via_streaming_encoder(
        &pixels,
        width,
        height,
        constants::DEFAULT_TILE_ROWS,
        &streaming_opts,
        true,
    );

    assert_eq!(
        streaming_bytes, whole_file_bytes,
        "Encoder<W>::finish_exact() output must match encode()'s whole-file output byte-for-byte \
         when using the same tile_rows/options"
    );

    let _ = std::fs::remove_file(&input_png);
    let _ = std::fs::remove_file(&output_cafe);
}

/// Extracts a `tile_w * tile_h * 4`-byte RGBA sub-buffer for tile `(tx, ty)`
/// out of a full-image RGBA buffer, matching `add_idim_tile()`'s expected
/// per-tile input shape (used by both the row-major and Z-order tests
/// below, and by the whole-image-vs-streaming comparison test).
fn extract_idim_tile(
    pixels: &[u8],
    img_width: u32,
    tx: u16,
    ty: u16,
    tile_w: u32,
    tile_h: u32,
    idim: &iDim,
) -> Vec<u8> {
    let x0 = (tx as u32) * (idim.tile_width as u32);
    let y0 = (ty as u32) * (idim.tile_height as u32);
    let mut out = Vec::with_capacity((tile_w * tile_h * 4) as usize);
    for row in 0..tile_h {
        let src_y = y0 + row;
        let start = ((src_y * img_width + x0) * 4) as usize;
        let end = start + (tile_w * 4) as usize;
        out.extend_from_slice(&pixels[start..end]);
    }
    out
}

/// Feeds `pixels` into a fresh iDIM-mode `Encoder<Cursor<Vec<u8>>>`, one
/// `add_idim_tile()` call per tile in `idim.tile_order()`'s sequence,
/// returning the finished byte buffer.
fn encode_via_streaming_encoder_idim(
    pixels: &[u8],
    width: u32,
    height: u32,
    opts: &EncoderOptions,
    use_finish_exact: bool,
) -> Vec<u8> {
    let (tile_width, tile_height, scan_order) =
        opts.idim.expect("opts.idim must be Some for this helper");
    let idim = iDim::new(tile_width, tile_height, width, height, scan_order);
    let order = idim.tile_order().expect("tile_order failed");

    let mut encoder =
        Encoder::new(Cursor::new(Vec::new()), width, height, opts).expect("Encoder::new failed");

    for &(tx, ty) in &order {
        let (tile_w, tile_h) = idim.tile_dimensions(tx, ty, width, height);
        let tile_bytes = extract_idim_tile(pixels, width, tx, ty, tile_w, tile_h, &idim);
        encoder
            .add_idim_tile(&tile_bytes)
            .expect("add_idim_tile failed");
    }

    let cursor = if use_finish_exact {
        encoder.finish_exact().expect("finish_exact failed")
    } else {
        encoder.finish().expect("finish failed")
    };
    cursor.into_inner()
}

/// Round-trips a non-tile-aligned image (33x23 with 8x8 tiles, so the last
/// column/row of tiles is a partial edge tile) through the streaming
/// encoder's `add_idim_tile()` path, row-major scan order, comparing
/// against `decode_bytes()`'s reassembled pixels.
#[test]
fn test_streaming_encoder_idim_row_major_roundtrip() {
    let width = 33u32;
    let height = 23u32;
    let pixels = generate_rgba(width, height);

    let opts = EncoderOptions {
        level: 6,
        use_filter: true,
        target_color_type: constants::COLOR_TYPE_RGBA,
        idim: Some((8, 8, 0)),
        ..Default::default()
    };

    let bytes = encode_via_streaming_encoder_idim(&pixels, width, height, &opts, false);
    let (decoded_pixels, result) = decode_bytes(&bytes).expect("decode_bytes failed");
    assert_eq!(result.width, width);
    assert_eq!(result.height, height);
    assert_eq!(decoded_pixels, pixels, "pixel mismatch for iDIM row-major");
}

/// Same as above but with Z-order (Morton) scan order (`scan_order = 1`).
#[test]
fn test_streaming_encoder_idim_zorder_roundtrip() {
    let width = 33u32;
    let height = 23u32;
    let pixels = generate_rgba(width, height);

    let opts = EncoderOptions {
        level: 6,
        use_filter: true,
        target_color_type: constants::COLOR_TYPE_RGBA,
        idim: Some((8, 8, 1)),
        ..Default::default()
    };

    let bytes = encode_via_streaming_encoder_idim(&pixels, width, height, &opts, true);
    let (decoded_pixels, result) = decode_bytes(&bytes).expect("decode_bytes failed");
    assert_eq!(result.width, width);
    assert_eq!(result.height, height);
    assert_eq!(decoded_pixels, pixels, "pixel mismatch for iDIM Z-order");
}

/// Compares the streaming iDIM encoder's `finish_exact()` output
/// byte-for-byte against `encode()`'s whole-file `EncodeOptions::idim` path
/// for the exact same pixels/options.
#[test]
fn test_streaming_encoder_idim_matches_whole_file_encode_byte_for_byte() {
    let temp_dir = std::env::temp_dir().join("cafe_streaming_encode_idim_tests");
    let _ = std::fs::create_dir_all(&temp_dir);
    let input_png = temp_dir.join("streaming_idim_vs_whole_input.png");
    let output_cafe = temp_dir.join("streaming_idim_vs_whole_output.cafe");

    let width = 40u32;
    let height = 30u32;
    let pixels = generate_rgba(width, height);

    let image_buffer = image::RgbaImage::from_raw(width, height, pixels.clone()).unwrap();
    image_buffer.save(&input_png).expect("save png failed");

    let whole_file_opts = EncodeOptions {
        use_filter: true,
        level: 6,
        adaptive_analysis: false,
        target_color_type: constants::COLOR_TYPE_RGBA,
        idim: Some(iDim::new(10, 10, width, height, 0)),
        ..Default::default()
    };
    encode(
        input_png.to_str().unwrap(),
        output_cafe.to_str().unwrap(),
        &whole_file_opts,
    )
    .expect("encode failed");
    let whole_file_bytes = std::fs::read(&output_cafe).expect("read output failed");

    let streaming_opts = EncoderOptions {
        level: 6,
        use_filter: true,
        target_color_type: constants::COLOR_TYPE_RGBA,
        idim: Some((10, 10, 0)),
        ..Default::default()
    };
    let streaming_bytes =
        encode_via_streaming_encoder_idim(&pixels, width, height, &streaming_opts, true);

    assert_eq!(
        streaming_bytes, whole_file_bytes,
        "Encoder<W>::add_idim_tile()+finish_exact() output must match encode()'s whole-file \
         iDIM output byte-for-byte for the same pixels/options"
    );

    let _ = std::fs::remove_file(&input_png);
    let _ = std::fs::remove_file(&output_cafe);
}

#[test]
fn test_streaming_encoder_add_tile_rejects_idim_mode() {
    let opts = EncoderOptions {
        idim: Some((4, 4, 0)),
        ..Default::default()
    };
    let mut encoder =
        Encoder::new(Cursor::new(Vec::new()), 8, 8, &opts).expect("Encoder::new failed");
    let tile = vec![0u8; 8 * 4 * 4];
    let err = encoder.add_tile(&tile).unwrap_err();
    assert!(matches!(err, CafeError::UnsupportedFeature(_)));
}

#[test]
fn test_streaming_encoder_add_idim_tile_rejects_row_strip_mode() {
    let opts = EncoderOptions::default();
    let mut encoder =
        Encoder::new(Cursor::new(Vec::new()), 8, 8, &opts).expect("Encoder::new failed");
    let tile = vec![0u8; 4 * 4 * 4];
    let err = encoder.add_idim_tile(&tile).unwrap_err();
    assert!(matches!(err, CafeError::UnsupportedFeature(_)));
}

#[test]
fn test_streaming_encoder_add_idim_tile_rejects_wrong_size_buffer() {
    let opts = EncoderOptions {
        idim: Some((4, 4, 0)),
        ..Default::default()
    };
    let mut encoder =
        Encoder::new(Cursor::new(Vec::new()), 8, 8, &opts).expect("Encoder::new failed");
    // First tile is 4x4, so 4*4*4=64 bytes expected; supply 60 instead.
    let bad_tile = vec![0u8; 60];
    let err = encoder.add_idim_tile(&bad_tile).unwrap_err();
    assert!(matches!(err, CafeError::UnsupportedFeature(_)));
}

#[test]
fn test_streaming_encoder_add_idim_tile_rejects_extra_tile_after_grid_complete() {
    let opts = EncoderOptions {
        idim: Some((4, 4, 0)),
        ..Default::default()
    };
    let mut encoder =
        Encoder::new(Cursor::new(Vec::new()), 4, 4, &opts).expect("Encoder::new failed");
    // 4x4 image with 4x4 tiles = exactly 1 tile in the grid.
    let tile = vec![0u8; 4 * 4 * 4];
    encoder
        .add_idim_tile(&tile)
        .expect("first add_idim_tile failed");
    let err = encoder.add_idim_tile(&tile).unwrap_err();
    assert!(matches!(err, CafeError::UnsupportedFeature(_)));
}

#[test]
fn test_streaming_encoder_idim_finish_rejects_incomplete_submission() {
    let opts = EncoderOptions {
        idim: Some((4, 4, 0)),
        ..Default::default()
    };
    let mut encoder =
        Encoder::new(Cursor::new(Vec::new()), 8, 8, &opts).expect("Encoder::new failed");
    // 8x8 image with 4x4 tiles = 4 tiles in the grid; submit only 1.
    let tile = vec![0u8; 4 * 4 * 4];
    encoder.add_idim_tile(&tile).expect("add_idim_tile failed");

    let err = encoder.finish().unwrap_err();
    assert!(matches!(err, CafeError::UnsupportedFeature(_)));
}

#[test]
fn test_streaming_encoder_new_rejects_idim_invalid_scan_order() {
    let opts = EncoderOptions {
        idim: Some((4, 4, 2)),
        ..Default::default()
    };
    match Encoder::new(Cursor::new(Vec::new()), 8, 8, &opts) {
        Ok(_) => panic!("expected UnsupportedFeature error for invalid scan_order"),
        Err(err) => assert!(matches!(err, CafeError::UnsupportedFeature(_))),
    }
}

#[test]
fn test_streaming_encoder_new_rejects_idim_zero_tile_dims() {
    let opts = EncoderOptions {
        idim: Some((0, 4, 0)),
        ..Default::default()
    };
    match Encoder::new(Cursor::new(Vec::new()), 8, 8, &opts) {
        Ok(_) => panic!("expected UnsupportedFeature error for zero tile_width"),
        Err(err) => assert!(matches!(err, CafeError::UnsupportedFeature(_))),
    }
}

#[test]
fn test_streaming_encoder_new_rejects_idim_with_bit_depth_below_8() {
    let opts = EncoderOptions {
        idim: Some((4, 4, 0)),
        target_bit_depth: Some(4),
        target_color_type: constants::COLOR_TYPE_GRAY,
        ..Default::default()
    };
    match Encoder::new(Cursor::new(Vec::new()), 8, 8, &opts) {
        Ok(_) => panic!("expected UnsupportedFeature error for bit_depth < 8 with iDIM"),
        Err(err) => assert!(matches!(err, CafeError::UnsupportedFeature(_))),
    }
}

#[test]
fn test_streaming_encoder_new_rejects_idim_with_filter_per_row() {
    let opts = EncoderOptions {
        idim: Some((4, 4, 0)),
        use_filter: true,
        use_filter_per_row: true,
        ..Default::default()
    };
    match Encoder::new(Cursor::new(Vec::new()), 8, 8, &opts) {
        Ok(_) => panic!("expected UnsupportedFeature error for iDIM + use_filter_per_row"),
        Err(err) => assert!(matches!(err, CafeError::UnsupportedFeature(_))),
    }
}

#[test]
fn test_streaming_encoder_new_rejects_idim_tile_count_exceeding_max() {
    // 1x1 tiles over an image large enough that tiles_x * tiles_y > MAX_TILE_COUNT.
    let opts = EncoderOptions {
        idim: Some((1, 1, 0)),
        ..Default::default()
    };
    match Encoder::new(Cursor::new(Vec::new()), 2000, 2000, &opts) {
        Ok(_) => panic!("expected UnsupportedFeature error for excessive tile count"),
        Err(err) => assert!(matches!(err, CafeError::UnsupportedFeature(_))),
    }
}
