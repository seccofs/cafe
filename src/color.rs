//! Color conversion and sample manipulation (bit-depth, packing)
//!
//! Functions for conversion between RGBA and specific color types, handling
//! different bit depths (1, 2, 4, 8, 10, 12, 16, 32), sub-byte packing
//! (for bit_depth < 8), and support for sample formats (uint, float, half-float).

use crate::constants::*;
use crate::error::{CafeError, Result};

/// Computes bytes per pixel considering color type, bit depth AND sample format.
///
/// # Logic
/// - If sample_format = FLOAT (1): bit_depth is ignored, uses 32-bit (4 bytes per channel)
/// - If sample_format = HALF (2): bit_depth is ignored, uses 16-bit (2 bytes per channel)
/// - If sample_format = UINT (0): uses normal bit_depth
pub(crate) fn bytes_per_pixel_with_format(
    color_type: u8,
    bit_depth: u8,
    sample_format: u8,
) -> Option<usize> {
    // If sample_format is float, use a 32-bit container
    // If sample_format is half, use a 16-bit container
    let effective_bit_depth = match sample_format {
        SAMPLE_FORMAT_FLOAT => 32,
        SAMPLE_FORMAT_HALF => 16,
        SAMPLE_FORMAT_UINT => bit_depth,
        _ => return None,
    };

    bytes_per_pixel(color_type, effective_bit_depth)
}

/// Computes bytes per pixel for a given color type and bit depth (validation + safety).
/// Used for buffer allocation and stride calculation.
/// Returns None if the combination is invalid (to avoid div by zero or overflow).
///
/// # Notes
/// - Bit depths 1, 2, 4 return 1 (packed, bytes_per_row must use packing)
/// - Bit depth 8 returns normal bpp (1, 2, 3, 4)
/// - Bit depths 10, 12, 16, 32 return 2 or 4 bytes (big-endian multi-byte)
pub(crate) fn bytes_per_pixel(color_type: u8, bit_depth: u8) -> Option<usize> {
    match color_type {
        COLOR_TYPE_GRAY => {
            match bit_depth {
                1 | 2 | 4 => Some(1), // Packed into 1 byte, but stride calculated specially
                8 => Some(1),
                10 | 12 | 16 => Some(2), // Big-endian 16-bit container
                32 => Some(4),
                _ => None,
            }
        }
        COLOR_TYPE_RGB => {
            match bit_depth {
                8 => Some(3),            // R, G, B
                10 | 12 | 16 => Some(6), // 3 channels × 2 bytes big-endian
                32 => Some(12),          // 3 channels × 4 bytes big-endian
                1 | 2 | 4 => None,       // RGB cannot be sub-byte
                _ => None,
            }
        }
        COLOR_TYPE_INDEXED => {
            // Palette: baseline is 1, but special packing for 1, 2, 4
            match bit_depth {
                1 | 2 | 4 | 8 => Some(1),
                _ => None,
            }
        }
        COLOR_TYPE_GRAY_ALPHA => {
            match bit_depth {
                1 | 2 | 4 => Some(1),    // Packed (Gray + Alpha share a byte)
                8 => Some(2),            // Gray, Alpha
                10 | 12 | 16 => Some(4), // 2 channels × 2 bytes big-endian
                32 => Some(8),
                _ => None,
            }
        }
        COLOR_TYPE_RGBA => {
            match bit_depth {
                8 => Some(4),            // R, G, B, A
                10 | 12 | 16 => Some(8), // 4 channels × 2 bytes big-endian
                32 => Some(16),          // 4 channels × 4 bytes big-endian
                1 | 2 | 4 => None,       // RGBA cannot be sub-byte
                _ => None,
            }
        }
        _ => None, // Unknown color type
    }
}

/// Number of channels (samples per pixel) for each color type, used to
/// size the float/half sample_format conversion (section 4.1).
pub(crate) fn samples_per_pixel(color_type: u8) -> Option<usize> {
    match color_type {
        COLOR_TYPE_GRAY => Some(1),
        COLOR_TYPE_RGB => Some(3),
        COLOR_TYPE_GRAY_ALPHA => Some(2),
        COLOR_TYPE_RGBA => Some(4),
        _ => None,
    }
}

/// Wrapper for convert_rgba_to_color_type that accepts a sample_format.
/// Converts the sample values according to the specified sample_format.
/// The color conversion operates on 8-bit values; each u8 sample is then
/// serialized as IEEE 754 float (32 bits) or half-float (16 bits).
pub(crate) fn convert_rgba_to_color_type_with_format(
    rgba: &[u8],
    width: u32,
    height: u32,
    target_color_type: u8,
    target_bit_depth: u8,
    sample_format: u8,
) -> Result<Vec<u8>> {
    // Validates that the bit depth declares a consistent container (section 4.1):
    // float always uses 32 bits/sample, half always 16 bits/sample.
    match sample_format {
        SAMPLE_FORMAT_FLOAT if target_bit_depth != 32 => {
            return Err(CafeError::UnsupportedFeature(format!(
                "Sample format FLOAT requires bit_depth 32, got {target_bit_depth}"
            )));
        }
        SAMPLE_FORMAT_HALF if target_bit_depth != 16 => {
            return Err(CafeError::UnsupportedFeature(format!(
                "Sample format HALF requires bit_depth 16, got {target_bit_depth}"
            )));
        }
        _ => {}
    }

    // First, convert from RGBA to the desired color type at 8 bits/sample,
    // then serialize each sample in the floating-point format.
    let intermediate = convert_rgba_to_color_type(rgba, width, height, target_color_type, 8)?;

    // If sample_format is float or half, convert the values
    match sample_format {
        SAMPLE_FORMAT_FLOAT => {
            // Converts 8-bit values to IEEE 754 32-bit big-endian float
            // Each original value becomes 4 bytes
            let mut result = Vec::with_capacity(intermediate.len() * 4);
            for &value in &intermediate {
                let float_val = u8_to_float(value);
                result.extend_from_slice(&float_val.to_bits().to_be_bytes());
            }
            Ok(result)
        }
        SAMPLE_FORMAT_HALF => {
            // Converts 8-bit values to half-float 16-bit big-endian
            // Each original value becomes 2 bytes
            let mut result = Vec::with_capacity(intermediate.len() * 2);
            for &value in &intermediate {
                let half_val = u8_to_half(value);
                result.extend_from_slice(&half_val.to_be_bytes());
            }
            Ok(result)
        }
        SAMPLE_FORMAT_UINT => {
            // No additional conversion
            Ok(intermediate)
        }
        _ => Err(CafeError::UnsupportedFeature(format!(
            "Sample format {} not supported",
            sample_format
        ))),
    }
}

/// Converts an RGBA image to a specific color type.
/// Fails if the color type is not supported or if there is non-tolerated data loss.
/// Used in encoding to convert input images to a target color type (with fallback to RGBA).
///
/// Supports:
/// - COLOR_TYPE_GRAY (0): bit_depth 1, 2, 4, 8
/// - COLOR_TYPE_RGB (2): bit_depth 8, 10, 12, 16, 32 (NOT 1,2,4 - 3 channels don't fit)
/// - COLOR_TYPE_INDEXED (3): bit_depth 1, 2, 4, 8 (but not reflected here, handled by encode_indexed)
/// - COLOR_TYPE_GRAY_ALPHA (4): bit_depth 1, 2, 4, 8
/// - COLOR_TYPE_RGBA (6): bit_depth 8, 10, 12, 16, 32 (NOT 1,2,4 - 4 channels don't fit)
///
/// # Safety
/// - Validates dimensions (width > 0, height > 0) and avoids overflow in allocation
/// - Checks that the conversion is reversible (no loss of information)
/// - For bit_depth < 8: reduces values before packing (lossless)
pub(crate) fn convert_rgba_to_color_type(
    rgba: &[u8],
    width: u32,
    height: u32,
    target_color_type: u8,
    target_bit_depth: u8,
) -> Result<Vec<u8>> {
    // Validation: prevents division by zero and overflow
    if width == 0 || height == 0 {
        return Err(CafeError::UnsupportedFeature(
            "Invalid dimensions para color conversion".into(),
        ));
    }

    // Validates that color_type and bit_depth are compatible
    match target_color_type {
        COLOR_TYPE_RGB | COLOR_TYPE_RGBA
            if (target_bit_depth == 1 || target_bit_depth == 2 || target_bit_depth == 4) =>
        {
            return Err(CafeError::UnsupportedFeature(format!(
                "Color type {target_color_type} does not support bit depth {} (requires 8+)",
                target_bit_depth
            )));
        }
        _ => {}
    }

    let bpp = bytes_per_pixel(target_color_type, target_bit_depth)
        .ok_or_else(|| {
            CafeError::UnsupportedFeature(format!(
                "Conversion para color type {target_color_type}, bit depth {target_bit_depth} not supported"
            ))
        })?;

    // Computes the output buffer size with overflow validation (section 12.1)
    let pixel_count_u64 = (width as u64).checked_mul(height as u64).ok_or_else(|| {
        CafeError::UnsupportedFeature(
            "Image dimensions causam overflow in conversion de cor".into(),
        )
    })?;

    // SECURITY: Validates that pixel_count fits in usize (especially important on 32-bit platforms)
    if pixel_count_u64 > (usize::MAX as u64) {
        return Err(CafeError::UnsupportedFeature(
            "Image dimensions excedem limite de memory allocation".into(),
        ));
    }
    let pixel_count = pixel_count_u64 as usize;

    // For bit_depth < 8, bpp=1 but data is emitted packed on the row
    // Final size is computed by bytes_per_row_for_bit_depth in the row
    // Here we pre-allocate conservatively
    let out_capacity = if target_bit_depth < 8 {
        // Each row has ceil(width * bit_depth / 8) bytes
        // Total: height * ceil(width * bit_depth / 8)
        let bytes_per_row = bytes_per_row_for_bit_depth(width, target_bit_depth)?;
        (height as usize)
            .checked_mul(bytes_per_row)
            .ok_or_else(|| {
                CafeError::UnsupportedFeature("Buffer size overflow in conversion".into())
            })?
    } else {
        pixel_count.checked_mul(bpp).ok_or_else(|| {
            CafeError::UnsupportedFeature("Output buffer da color conversion would overflow".into())
        })?
    };

    match target_color_type {
        COLOR_TYPE_GRAY => {
            let mut out = Vec::with_capacity(out_capacity);

            // Converts RGBA → Gray (Y = 0.299*R + 0.587*G + 0.114*B)
            if target_bit_depth <= 8 {
                // Sub-byte (1,2,4) and 8-bit: process as 8-bit values, then pack if needed
                let mut all_samples = Vec::new();
                for chunk in rgba.chunks_exact(4) {
                    let r = chunk[0] as u32;
                    let g = chunk[1] as u32;
                    let b = chunk[2] as u32;
                    let gray = ((299 * r + 587 * g + 114 * b) / 1000).min(255) as u8;

                    // If bit_depth < 8, reduce
                    let gray_reduced = reduce_sample_8_to_n_bits(gray, target_bit_depth)?;
                    all_samples.push(gray_reduced);
                }

                // If bit_depth < 8, pack row by row
                if target_bit_depth < 8 {
                    for row in 0..height as usize {
                        let row_start = row * width as usize;
                        let row_end = row_start + width as usize;
                        if row_end > all_samples.len() {
                            return Err(CafeError::TruncatedFile(
                                "convert_rgba_to_color_type: insufficient samples after reduction"
                                    .into(),
                            ));
                        }
                        let row_samples = &all_samples[row_start..row_end];
                        let packed =
                            pack_samples_row(row_samples, target_bit_depth, width as usize, 1)?;
                        out.extend_from_slice(&packed);
                    }
                } else {
                    out.extend_from_slice(&all_samples);
                }
            } else {
                // Multi-byte (10,12,16,32): expands 8-bit values to N-bit big-endian
                for chunk in rgba.chunks_exact(4) {
                    let r = chunk[0] as u32;
                    let g = chunk[1] as u32;
                    let b = chunk[2] as u32;
                    let gray = ((299 * r + 587 * g + 114 * b) / 1000).min(255) as u8;

                    match target_bit_depth {
                        10 | 12 | 16 => {
                            let expanded = expand_sample_8_to_n_bits(gray, target_bit_depth)?;
                            out.extend_from_slice(&expanded.to_be_bytes());
                        }
                        32 => {
                            let expanded = expand_sample_8_to_32bit(gray);
                            out.extend_from_slice(&expanded.to_be_bytes());
                        }
                        _ => {
                            return Err(CafeError::UnsupportedFeature(format!(
                                "Gray bit_depth {} not supported",
                                target_bit_depth
                            )))
                        }
                    }
                }
            }

            Ok(out)
        }
        COLOR_TYPE_RGB => {
            // RGB does not support bit_depth < 8 (validated above)
            let mut out = Vec::with_capacity(out_capacity);

            if target_bit_depth == 8 {
                // RGB 8-bit: no expansion
                for chunk in rgba.chunks_exact(4) {
                    out.push(chunk[0]); // R
                    out.push(chunk[1]); // G
                    out.push(chunk[2]); // B
                }
            } else {
                // RGB multi-byte (10,12,16,32): expands each channel
                for chunk in rgba.chunks_exact(4) {
                    let r = chunk[0];
                    let g = chunk[1];
                    let b = chunk[2];

                    match target_bit_depth {
                        10 | 12 | 16 => {
                            let r_exp = expand_sample_8_to_n_bits(r, target_bit_depth)?;
                            let g_exp = expand_sample_8_to_n_bits(g, target_bit_depth)?;
                            let b_exp = expand_sample_8_to_n_bits(b, target_bit_depth)?;
                            out.extend_from_slice(&r_exp.to_be_bytes());
                            out.extend_from_slice(&g_exp.to_be_bytes());
                            out.extend_from_slice(&b_exp.to_be_bytes());
                        }
                        32 => {
                            let r_exp = expand_sample_8_to_32bit(r);
                            let g_exp = expand_sample_8_to_32bit(g);
                            let b_exp = expand_sample_8_to_32bit(b);
                            out.extend_from_slice(&r_exp.to_be_bytes());
                            out.extend_from_slice(&g_exp.to_be_bytes());
                            out.extend_from_slice(&b_exp.to_be_bytes());
                        }
                        _ => {
                            return Err(CafeError::UnsupportedFeature(format!(
                                "RGB bit_depth {} not supported",
                                target_bit_depth
                            )))
                        }
                    }
                }
            }

            Ok(out)
        }
        COLOR_TYPE_GRAY_ALPHA => {
            let mut out = Vec::with_capacity(out_capacity);

            // Converts RGBA → Gray+Alpha (Y, A)
            if target_bit_depth <= 8 {
                // Sub-byte (1,2,4) and 8-bit: process as 8-bit values, then pack if needed
                let mut all_samples = Vec::new();
                for chunk in rgba.chunks_exact(4) {
                    let r = chunk[0] as u32;
                    let g = chunk[1] as u32;
                    let b = chunk[2] as u32;
                    let a = chunk[3];
                    let gray = ((299 * r + 587 * g + 114 * b) / 1000).min(255) as u8;

                    // Reduce if necessary
                    let gray_reduced = reduce_sample_8_to_n_bits(gray, target_bit_depth)?;
                    let a_reduced = reduce_sample_8_to_n_bits(a, target_bit_depth)?;
                    all_samples.push(gray_reduced);
                    all_samples.push(a_reduced);
                }

                // If bit_depth < 8, pack row by row with bpp=2 (gray + alpha)
                if target_bit_depth < 8 {
                    for row in 0..height as usize {
                        let row_start = row * width as usize * 2;
                        let row_end = row_start + width as usize * 2;
                        if row_end > all_samples.len() {
                            return Err(CafeError::TruncatedFile(
                                "convert_rgba_to_color_type: insufficient samples after reduction (GA)"
                                    .into(),
                            ));
                        }
                        let row_samples = &all_samples[row_start..row_end];
                        let packed =
                            pack_samples_row(row_samples, target_bit_depth, width as usize, 2)?;
                        out.extend_from_slice(&packed);
                    }
                } else {
                    out.extend_from_slice(&all_samples);
                }
            } else {
                // Multi-byte (10,12,16,32): expands each sample
                for chunk in rgba.chunks_exact(4) {
                    let r = chunk[0] as u32;
                    let g = chunk[1] as u32;
                    let b = chunk[2] as u32;
                    let a = chunk[3];
                    let gray = ((299 * r + 587 * g + 114 * b) / 1000).min(255) as u8;

                    match target_bit_depth {
                        10 | 12 | 16 => {
                            let gray_exp = expand_sample_8_to_n_bits(gray, target_bit_depth)?;
                            let a_exp = expand_sample_8_to_n_bits(a, target_bit_depth)?;
                            out.extend_from_slice(&gray_exp.to_be_bytes());
                            out.extend_from_slice(&a_exp.to_be_bytes());
                        }
                        32 => {
                            let gray_exp = expand_sample_8_to_32bit(gray);
                            let a_exp = expand_sample_8_to_32bit(a);
                            out.extend_from_slice(&gray_exp.to_be_bytes());
                            out.extend_from_slice(&a_exp.to_be_bytes());
                        }
                        _ => {
                            return Err(CafeError::UnsupportedFeature(format!(
                                "Gray+Alpha bit_depth {} not supported",
                                target_bit_depth
                            )))
                        }
                    }
                }
            }

            Ok(out)
        }
        COLOR_TYPE_RGBA => {
            // RGBA does not support bit_depth < 8 (validated above)
            let mut out = Vec::with_capacity(out_capacity);

            if target_bit_depth == 8 {
                // RGBA 8-bit: no conversion
                out.extend_from_slice(rgba);
            } else {
                // RGBA multi-byte (10,12,16,32): expands each channel
                for chunk in rgba.chunks_exact(4) {
                    let r = chunk[0];
                    let g = chunk[1];
                    let b = chunk[2];
                    let a = chunk[3];

                    match target_bit_depth {
                        10 | 12 | 16 => {
                            let r_exp = expand_sample_8_to_n_bits(r, target_bit_depth)?;
                            let g_exp = expand_sample_8_to_n_bits(g, target_bit_depth)?;
                            let b_exp = expand_sample_8_to_n_bits(b, target_bit_depth)?;
                            let a_exp = expand_sample_8_to_n_bits(a, target_bit_depth)?;
                            out.extend_from_slice(&r_exp.to_be_bytes());
                            out.extend_from_slice(&g_exp.to_be_bytes());
                            out.extend_from_slice(&b_exp.to_be_bytes());
                            out.extend_from_slice(&a_exp.to_be_bytes());
                        }
                        32 => {
                            let r_exp = expand_sample_8_to_32bit(r);
                            let g_exp = expand_sample_8_to_32bit(g);
                            let b_exp = expand_sample_8_to_32bit(b);
                            let a_exp = expand_sample_8_to_32bit(a);
                            out.extend_from_slice(&r_exp.to_be_bytes());
                            out.extend_from_slice(&g_exp.to_be_bytes());
                            out.extend_from_slice(&b_exp.to_be_bytes());
                            out.extend_from_slice(&a_exp.to_be_bytes());
                        }
                        _ => {
                            return Err(CafeError::UnsupportedFeature(format!(
                                "RGBA bit_depth {} not supported",
                                target_bit_depth
                            )))
                        }
                    }
                }
            }

            Ok(out)
        }
        _ => Err(CafeError::UnsupportedFeature(format!(
            "Color type {target_color_type} not supported"
        ))),
    }
}

/// Wrapper for convert_color_type_to_rgba that accepts a sample_format.
/// Converts values from the specified format (float/half) back to 8-bit before processing.
///
/// The input buffer contains `width × height × channels` samples, each with
/// 4 bytes (float) or 2 bytes (half). After reducing to 8 bits/sample, it delegates
/// the color conversion to `convert_color_type_to_rgba` with bit_depth 8.
pub(crate) fn convert_color_type_to_rgba_with_format(
    data: &[u8],
    width: u32,
    height: u32,
    color_type: u8,
    bit_depth: u8,
    sample_format: u8,
) -> Result<Vec<u8>> {
    let channels = samples_per_pixel(color_type).ok_or_else(|| {
        CafeError::UnsupportedFeature(format!(
            "convert_color_type_to_rgba_with_format: color type {color_type} sem canais definidos"
        ))
    })?;
    let sample_count = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(channels))
        .ok_or_else(|| {
            CafeError::UnsupportedFeature(
                "convert_color_type_to_rgba_with_format: overflow in sample count".into(),
            )
        })?;

    // If sample_format is float or half, convert back to 8-bit first
    let intermediate = match sample_format {
        SAMPLE_FORMAT_FLOAT => {
            // Converts IEEE 754 32-bit float back to 8-bit
            // Each 4 bytes represents 1 sample
            let mut result = Vec::with_capacity(sample_count);
            let mut offset = 0;
            for _ in 0..sample_count {
                if offset + 4 > data.len() {
                    return Err(CafeError::TruncatedFile(
                        "convert_color_type_to_rgba: insuficientes dados float".into(),
                    ));
                }
                let float_bits = u32::from_be_bytes([
                    data[offset],
                    data[offset + 1],
                    data[offset + 2],
                    data[offset + 3],
                ]);
                let float_val = f32::from_bits(float_bits);
                let u8_val = float_to_u8(float_val);
                result.push(u8_val);
                offset += 4;
            }
            result
        }
        SAMPLE_FORMAT_HALF => {
            // Converts half-float 16-bit back to 8-bit
            // Each 2 bytes represents 1 sample
            let mut result = Vec::with_capacity(sample_count);
            let mut offset = 0;
            for _ in 0..sample_count {
                if offset + 2 > data.len() {
                    return Err(CafeError::TruncatedFile(
                        "convert_color_type_to_rgba: insuficientes dados half".into(),
                    ));
                }
                let half_bits = u16::from_be_bytes([data[offset], data[offset + 1]]);
                let u8_val = half_to_u8(half_bits);
                result.push(u8_val);
                offset += 2;
            }
            result
        }
        SAMPLE_FORMAT_UINT => {
            let _ = bit_depth; // 8-bit data per sample (after packing/unpacking in convert_color_type_to_rgba)
            data.to_vec()
        }
        _ => {
            return Err(CafeError::UnsupportedFeature(format!(
                "Sample format {} not supported",
                sample_format
            )))
        }
    };

    // After the float/half → u8 conversion, the data is 8-bit samples;
    // the color conversion (sub-byte packing, 16/32 expansion, etc.) uses 8 bits.
    convert_color_type_to_rgba(&intermediate, width, height, color_type, 8)
}

/// Converts a buffer of a specific color type back to RGBA.
/// Used in decoding to convert the decompressed image to a standard format.
///
/// # Safety
/// - Validates buffer size vs. expected dimensions
/// - Avoids out-of-bounds indexing
/// - No overflow in allocation (pre-allocated with exact capacity)
pub(crate) fn convert_color_type_to_rgba(
    data: &[u8],
    width: u32,
    height: u32,
    color_type: u8,
    bit_depth: u8,
) -> Result<Vec<u8>> {
    // Dimensions validation
    if width == 0 || height == 0 {
        return Err(CafeError::UnsupportedFeature(
            "Invalid dimensions para color conversion".into(),
        ));
    }

    // For bit_depth < 8, we need to unpack data row-by-row (sub-byte packing)
    // For bit_depth > 8, we need to de-expand multi-byte big-endian values

    // Unpacks/decompacts as needed
    let unpacked_data = if bit_depth < 8 {
        let mut unpacked = Vec::new();
        match color_type {
            COLOR_TYPE_GRAY => {
                // Unpacks each row with bpp=1
                for row in 0..height as usize {
                    let bytes_per_row = bytes_per_row_for_bit_depth(width, bit_depth)?;
                    let row_start = row * bytes_per_row;
                    let row_end = row_start + bytes_per_row;
                    if row_end > data.len() {
                        return Err(CafeError::TruncatedFile(
                            "convert_color_type_to_rgba: insuficientes dados compactados (GRAY)"
                                .into(),
                        ));
                    }
                    let row_packed = &data[row_start..row_end];
                    let row_unpacked =
                        unpack_samples_row(row_packed, bit_depth, width as usize, 1)?;
                    unpacked.extend_from_slice(&row_unpacked);
                }
                unpacked
            }
            COLOR_TYPE_GRAY_ALPHA => {
                // Unpacks each row with bpp=2
                for row in 0..height as usize {
                    let bytes_per_row =
                        bytes_per_row_for_bit_depth(width, bit_depth).and_then(|_bpr| {
                            // Each pixel has 2 samples, so bytes_per_row is for 2*width samples
                            (2 * width as usize)
                                .checked_mul(bit_depth as usize)
                                .and_then(|b| b.div_ceil(8).checked_add(0))
                                .ok_or(CafeError::TruncatedFile(
                                    "GA row calculation overflow".into(),
                                ))
                        })?;
                    let row_start = row * bytes_per_row;
                    let row_end = row_start + bytes_per_row;
                    if row_end > data.len() {
                        return Err(CafeError::TruncatedFile(
                            "convert_color_type_to_rgba: insuficientes dados compactados (GA)"
                                .into(),
                        ));
                    }
                    let row_packed = &data[row_start..row_end];
                    let row_unpacked =
                        unpack_samples_row(row_packed, bit_depth, width as usize, 2)?;
                    unpacked.extend_from_slice(&row_unpacked);
                }
                unpacked
            }
            _ => data.to_vec(), // Other types do not support bit_depth < 8
        }
    } else if bit_depth > 8 {
        // Multi-byte (10,12,16,32): de-expands big-endian values back to 8-bit
        let mut decompacted = Vec::new();

        match color_type {
            COLOR_TYPE_GRAY => {
                // Reads each big-endian value and compresses to 8-bit
                let mut offset = 0;
                for _ in 0..(width as usize * height as usize) {
                    match bit_depth {
                        10 | 12 | 16 => {
                            let val = read_u16_be(data, offset)?;
                            let compressed = compress_sample_n_to_8bits(val, bit_depth)?;
                            decompacted.push(compressed);
                            offset += 2;
                        }
                        32 => {
                            let val = read_u32_be(data, offset)?;
                            let compressed = compress_sample_32bit_to_8(val);
                            decompacted.push(compressed);
                            offset += 4;
                        }
                        _ => unreachable!(),
                    }
                }
                decompacted
            }
            COLOR_TYPE_RGB => {
                // Reads 3 big-endian values per pixel
                let mut offset = 0;
                for _ in 0..(width as usize * height as usize) {
                    match bit_depth {
                        10 | 12 | 16 => {
                            let r = read_u16_be(data, offset)?;
                            let g = read_u16_be(data, offset + 2)?;
                            let b = read_u16_be(data, offset + 4)?;
                            decompacted.push(compress_sample_n_to_8bits(r, bit_depth)?);
                            decompacted.push(compress_sample_n_to_8bits(g, bit_depth)?);
                            decompacted.push(compress_sample_n_to_8bits(b, bit_depth)?);
                            offset += 6;
                        }
                        32 => {
                            let r = read_u32_be(data, offset)?;
                            let g = read_u32_be(data, offset + 4)?;
                            let b = read_u32_be(data, offset + 8)?;
                            decompacted.push(compress_sample_32bit_to_8(r));
                            decompacted.push(compress_sample_32bit_to_8(g));
                            decompacted.push(compress_sample_32bit_to_8(b));
                            offset += 12;
                        }
                        _ => unreachable!(),
                    }
                }
                decompacted
            }
            COLOR_TYPE_GRAY_ALPHA => {
                // Reads 2 big-endian values per pixel
                let mut offset = 0;
                for _ in 0..(width as usize * height as usize) {
                    match bit_depth {
                        10 | 12 | 16 => {
                            let gray = read_u16_be(data, offset)?;
                            let alpha = read_u16_be(data, offset + 2)?;
                            decompacted.push(compress_sample_n_to_8bits(gray, bit_depth)?);
                            decompacted.push(compress_sample_n_to_8bits(alpha, bit_depth)?);
                            offset += 4;
                        }
                        32 => {
                            let gray = read_u32_be(data, offset)?;
                            let alpha = read_u32_be(data, offset + 4)?;
                            decompacted.push(compress_sample_32bit_to_8(gray));
                            decompacted.push(compress_sample_32bit_to_8(alpha));
                            offset += 8;
                        }
                        _ => unreachable!(),
                    }
                }
                decompacted
            }
            COLOR_TYPE_RGBA => {
                // Reads 4 big-endian values per pixel
                let mut offset = 0;
                for _ in 0..(width as usize * height as usize) {
                    match bit_depth {
                        10 | 12 | 16 => {
                            let r = read_u16_be(data, offset)?;
                            let g = read_u16_be(data, offset + 2)?;
                            let b = read_u16_be(data, offset + 4)?;
                            let a = read_u16_be(data, offset + 6)?;
                            decompacted.push(compress_sample_n_to_8bits(r, bit_depth)?);
                            decompacted.push(compress_sample_n_to_8bits(g, bit_depth)?);
                            decompacted.push(compress_sample_n_to_8bits(b, bit_depth)?);
                            decompacted.push(compress_sample_n_to_8bits(a, bit_depth)?);
                            offset += 8;
                        }
                        32 => {
                            let r = read_u32_be(data, offset)?;
                            let g = read_u32_be(data, offset + 4)?;
                            let b = read_u32_be(data, offset + 8)?;
                            let a = read_u32_be(data, offset + 12)?;
                            decompacted.push(compress_sample_32bit_to_8(r));
                            decompacted.push(compress_sample_32bit_to_8(g));
                            decompacted.push(compress_sample_32bit_to_8(b));
                            decompacted.push(compress_sample_32bit_to_8(a));
                            offset += 16;
                        }
                        _ => unreachable!(),
                    }
                }
                decompacted
            }
            _ => data.to_vec(), // Other types do not yet support bit_depth > 8
        }
    } else {
        data.to_vec()
    };

    // Now processes the unpacked/decompacted data (always 8-bit samples)
    let effective_bit_depth = 8; // After unpacking, we have 8-bit samples

    let _bpp = bytes_per_pixel(color_type, effective_bit_depth).ok_or_else(|| {
        CafeError::UnsupportedFeature(format!(
            "Conversion do color type {color_type}, bit depth {effective_bit_depth} not supported"
        ))
    })?;

    let pixel_count_u64 = (width as u64).checked_mul(height as u64).ok_or_else(|| {
        CafeError::UnsupportedFeature(
            "Image dimensions causam overflow in conversion de cor".into(),
        )
    })?;

    // SECURITY: Validates that pixel_count fits in usize
    if pixel_count_u64 > (usize::MAX as u64) {
        return Err(CafeError::UnsupportedFeature(
            "Image dimensions excedem limite de memory allocation".into(),
        ));
    }
    let pixel_count = pixel_count_u64 as usize;

    // Allocates output (always RGBA = 4 bytes per pixel)
    let out_size = pixel_count
        .checked_mul(4)
        .ok_or_else(|| CafeError::UnsupportedFeature("Output buffer RGBA would overflow".into()))?;

    let mut out = Vec::with_capacity(out_size);

    match color_type {
        COLOR_TYPE_GRAY => {
            // Gray → RGBA (replicates gray to RGB, alpha = 0xFF)
            for &gray in &unpacked_data[..pixel_count.min(unpacked_data.len())] {
                out.push(gray); // R
                out.push(gray); // G
                out.push(gray); // B
                out.push(0xFF); // A (opaque)
            }
            Ok(out)
        }
        COLOR_TYPE_RGB => {
            // RGB → RGBA (adds alpha = 0xFF)
            for chunk in unpacked_data.chunks_exact(3) {
                out.push(chunk[0]); // R
                out.push(chunk[1]); // G
                out.push(chunk[2]); // B
                out.push(0xFF); // A (opaque)
            }
            Ok(out)
        }
        COLOR_TYPE_GRAY_ALPHA => {
            // Gray+Alpha → RGBA (replicates gray to RGB, keeps alpha)
            for chunk in unpacked_data.chunks_exact(2) {
                let gray = chunk[0];
                let alpha = chunk[1];
                out.push(gray); // R
                out.push(gray); // G
                out.push(gray); // B
                out.push(alpha); // A
            }
            Ok(out)
        }
        COLOR_TYPE_RGBA => {
            // No conversion: returns as-is
            out.extend_from_slice(&unpacked_data);
            Ok(out)
        }
        _ => Err(CafeError::UnsupportedFeature(format!(
            "Color type {color_type} not supported"
        ))),
    }
}

// Helper functions for sample manipulation

pub(crate) fn bytes_per_row_for_bit_depth(width: u32, bit_depth: u8) -> Result<usize> {
    if bit_depth != 1 && bit_depth != 2 && bit_depth != 4 && bit_depth != 8 {
        return Err(CafeError::UnsupportedFeature(format!(
            "bit_depth {} not allowed para packing (only 1, 2, 4, 8)",
            bit_depth
        )));
    }
    if bit_depth == 8 {
        return Ok(width as usize);
    }
    // Computes ceil(width * bit_depth / 8) with overflow protection
    let bits_total = (width as u64)
        .checked_mul(bit_depth as u64)
        .ok_or_else(|| {
            CafeError::TruncatedFile(
                "Calculation of bytes_per_row overflow: width × bit_depth muito grande".into(),
            )
        })?;
    let bytes = bits_total.div_ceil(8) as usize;
    Ok(bytes)
}

/// Reduces an 8-bit value to N bits, preparing it for packing.
/// Used when converting RGBA to Gray/Gray+Alpha with bit depth < 8.
///
/// Example: 8-bit → 4-bit: value 255 becomes 15 (255 >> 4)
///
/// # Safety
/// - Validates that bit_depth is valid (1, 2, 4)
/// - Uses safe arithmetic shift (no possible overflow)
pub(crate) fn reduce_sample_8_to_n_bits(value_8bit: u8, bit_depth: u8) -> Result<u8> {
    match bit_depth {
        1 => Ok(if value_8bit >= 128 { 1 } else { 0 }), // Threshold at mid-tone
        2 => Ok(value_8bit >> 6),                       // Reduces 8 → 2 bits
        4 => Ok(value_8bit >> 4),                       // Reduces 8 → 4 bits
        8 => Ok(value_8bit),                            // No reduction
        _ => Err(CafeError::UnsupportedFeature(format!(
            "reduce_sample: bit_depth {} not supported",
            bit_depth
        ))),
    }
}

/// Reads a big-endian u16 from a buffer.
///
/// # Safety
/// - Validates that the buffer has at least 2 bytes
/// - Returns Err if the buffer is too small
pub(crate) fn read_u16_be(buf: &[u8], offset: usize) -> Result<u16> {
    if offset + 2 > buf.len() {
        return Err(CafeError::TruncatedFile(
            "Buffer muito pequeno para ler u16 big-endian".into(),
        ));
    }
    Ok(u16::from_be_bytes([buf[offset], buf[offset + 1]]))
}

/// Reads a big-endian u32 from a buffer.
///
/// # Safety
/// - Validates that the buffer has at least 4 bytes
/// - Returns Err if the buffer is too small
pub(crate) fn read_u32_be(buf: &[u8], offset: usize) -> Result<u32> {
    if offset + 4 > buf.len() {
        return Err(CafeError::TruncatedFile(
            "Buffer muito pequeno para ler u32 big-endian".into(),
        ));
    }
    Ok(u32::from_be_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
    ]))
}

/// Expands a reduced N-bit value to 8 bits (inverse of reduce_sample_8_to_n_bits).
/// Used when converting a color type to RGBA after unpacking.
///
/// Example: 4-bit → 8-bit: value 15 becomes 255 (15 * 17)
///
/// # Safety
/// - Validates that bit_depth is valid (1, 2, 4)
#[allow(dead_code)]
pub(crate) fn expand_sample_n_to_8_bits(value_n_bit: u8, bit_depth: u8) -> Result<u8> {
    match bit_depth {
        1 => Ok(if value_n_bit != 0 { 255 } else { 0 }), // 1-bit: 0→0, 1→255
        2 => {
            // 2-bit: 0→0, 1→85, 2→170, 3→255
            Ok(value_n_bit.saturating_mul(85))
        }
        4 => {
            // 4-bit: multiply by 17 (255/15)
            Ok(value_n_bit.saturating_mul(17))
        }
        8 => Ok(value_n_bit), // No expansion
        _ => Err(CafeError::UnsupportedFeature(format!(
            "expand_sample: bit_depth {} not supported",
            bit_depth
        ))),
    }
}

/// Converts an 8-bit value to N-bit (10, 12, 16, 32).
/// Used when converting RGBA to color types with bit depth > 8.
///
/// Strategy: scales the value by multiplying to fill the full precision.
/// Example: 8-bit → 10-bit: value 255 becomes 1023 (255 * 4 + 3)
/// Example: 8-bit → 16-bit: value 255 becomes 65535 (255 * 257)
///
/// # Safety
/// - Validates that bit_depth is valid (10, 12, 16, 32)
pub(crate) fn expand_sample_8_to_n_bits(value_8bit: u8, bit_depth: u8) -> Result<u16> {
    match bit_depth {
        10 => {
            // 8-bit → 10-bit: multiply by (2^10-1)/(2^8-1) ≈ 4.0
            // Use intermediate u32 to avoid overflow
            let expanded = ((value_8bit as u32) * 1023 / 255) as u16;
            Ok(expanded)
        }
        12 => {
            // 8-bit → 12-bit: multiply by (2^12-1)/(2^8-1) ≈ 16.0
            // Use intermediate u32 to avoid overflow
            let expanded = ((value_8bit as u32) * 4095 / 255) as u16;
            Ok(expanded)
        }
        16 => {
            // 8-bit → 16-bit: multiply by (2^16-1)/(2^8-1) ≈ 257.0
            // Use intermediate u32 to avoid overflow
            let expanded = ((value_8bit as u32) * 65535 / 255) as u16;
            Ok(expanded)
        }
        _ => Err(CafeError::UnsupportedFeature(format!(
            "expand_sample_8_to_n_bits: bit_depth {} not supported",
            bit_depth
        ))),
    }
}

/// Converts an 8-bit value to 32-bit.
/// Used when converting RGBA to color types with bit depth 32.
///
/// # Safety
/// - Multiplies the 8-bit value to fill 32 bits
pub(crate) fn expand_sample_8_to_32bit(value_8bit: u8) -> u32 {
    // 8-bit → 32-bit: expands and fills the lower bits by repetition
    let val32 = (value_8bit as u32) << 24;
    val32 | (val32 >> 8) | (val32 >> 16) | (val32 >> 24)
}

/// Compresses an N-bit value to 8-bit.
/// Inverse of expand_sample_8_to_n_bits.
/// Example: 10-bit → 8-bit: value 1023 becomes 255
/// Example: 16-bit → 8-bit: value 65535 becomes 255
///
/// # Safety
/// - Validates that bit_depth is valid (10, 12, 16)
pub(crate) fn compress_sample_n_to_8bits(value_n_bit: u16, bit_depth: u8) -> Result<u8> {
    match bit_depth {
        10 => {
            // 10-bit → 8-bit: divide by 4 and truncate
            Ok((value_n_bit >> 2) as u8)
        }
        12 => {
            // 12-bit → 8-bit: divide by 16 and truncate
            Ok((value_n_bit >> 4) as u8)
        }
        16 => {
            // 16-bit → 8-bit: divide by 257 and truncate
            Ok((value_n_bit >> 8) as u8)
        }
        _ => Err(CafeError::UnsupportedFeature(format!(
            "compress_sample_n_to_8bits: bit_depth {} not supported",
            bit_depth
        ))),
    }
}

/// Compresses a 32-bit value to 8-bit.
/// Inverse of expand_sample_8_to_32bit.
pub(crate) fn compress_sample_32bit_to_8(value_32bit: u32) -> u8 {
    (value_32bit >> 24) as u8
}

/// Converts an 8-bit value (0-255) to half-float (fp16).
/// Used when converting RGBA to sample_format=HALF.
///
/// # Implementation
/// - Expands 8-bit to float (-1.0 to 1.0, or 0.0 to 1.0)
/// - Writes as big-endian half-float
///
/// # Safety
/// - Uses the `half` crate for correct IEEE 754 conversion
pub(crate) fn u8_to_half(value: u8) -> u16 {
    // Converts 0-255 to 0.0-1.0 in float
    let float_val = (value as f32) / 255.0;
    // Uses half::f16 for IEEE 754 conversion
    let half = half::f16::from_f32(float_val);
    // Returns as big-endian u16
    half.to_bits()
}

/// Converts half-float (fp16) to 8-bit (0-255).
/// Inverse of u8_to_half.
pub(crate) fn half_to_u8(half_bits: u16) -> u8 {
    // Reconstructs half-float from the bits
    let half = half::f16::from_bits(half_bits);
    // Converts to f32
    let float_val = half.to_f32();
    // Converts 0.0-1.0 to 0-255, with clipping
    let clamped = float_val.clamp(0.0, 1.0);
    (clamped * 255.0).round() as u8
}

/// Converts an 8-bit value to IEEE 754 float (32-bit).
/// Used when converting RGBA to sample_format=FLOAT.
///
/// # Implementation
/// - Expands 8-bit (0-255) to float (0.0-1.0)
pub(crate) fn u8_to_float(value: u8) -> f32 {
    (value as f32) / 255.0
}

/// Converts IEEE 754 float (32-bit) to 8-bit.
/// Inverse of u8_to_float.
pub(crate) fn float_to_u8(value: f32) -> u8 {
    let clamped = value.clamp(0.0, 1.0);
    (clamped * 255.0).round() as u8
}

/// Packs a row of values (1 byte each) into `bit_depth` bits per value,
/// MSB first, zero-padded at the end (section 4.1.1/4.1.2).
/// Applicable to: palette indices (type 3), mono samples of bit depth 1,2,4 (types 0, 4).
///
/// # Safety
/// - Validates that each value fits in bit_depth bits (max = 2^bit_depth - 1)
/// - Returns Err if value > maximum (does not silently truncate)
/// - Uses a mask to avoid overflow
pub(crate) fn pack_indices_row(row: &[u8], bit_depth: u8) -> Result<Vec<u8>> {
    if bit_depth == 8 {
        return Ok(row.to_vec());
    }

    if bit_depth != 1 && bit_depth != 2 && bit_depth != 4 {
        return Err(CafeError::UnsupportedFeature(format!(
            "pack_indices_row: bit_depth {} not supported (apenas 1, 2, 4, 8)",
            bit_depth
        )));
    }

    let max_value = (1u8 << bit_depth) - 1; // 2^bit_depth - 1
    let per_byte = 8 / bit_depth as usize;

    let bpr = (row.len() * bit_depth as usize).div_ceil(8);
    let mut out = vec![0u8; bpr];
    let mask = max_value;

    for (i, &idx) in row.iter().enumerate() {
        // Validation: each value must fit in bit_depth bits
        if idx > max_value {
            return Err(CafeError::UnsupportedFeature(format!(
                "Valor {} does not fit in {} bits (maximum {})",
                idx, bit_depth, max_value
            )));
        }

        let byte_idx = i / per_byte;
        let slot = i % per_byte; // 0 = most significant position within the byte
        let shift = 8 - bit_depth as usize * (slot + 1);
        out[byte_idx] |= (idx & mask) << shift;
    }
    Ok(out)
}

/// Packs generic samples (not only indices) into `bit_depth` bits.
/// Works for Grayscale and Grayscale+Alpha with bit depth 1, 2, 4.
///
/// # Parameters
/// - `samples`: 8-bit values (1 or 2 per pixel, already reduced if needed)
/// - `bit_depth`: 1, 2, 4, or 8
/// - `width`: number of pixels (not samples)
/// - `bpp`: bytes per pixel in the output data (normally 1 for packed)
///
/// # Safety
/// - Validates that (width × bpp) matches the expected packed buffer size
/// - Returns Err if bit_depth is invalid
pub(crate) fn pack_samples_row(
    samples: &[u8],
    bit_depth: u8,
    width: usize,
    bpp: usize,
) -> Result<Vec<u8>> {
    if bit_depth == 8 {
        // No packing needed, just copy
        if samples.len() < width * bpp {
            return Err(CafeError::TruncatedFile(
                "pack_samples_row: samples buffer curto para bit_depth=8".into(),
            ));
        }
        return Ok(samples[..width * bpp].to_vec());
    }

    if bit_depth != 1 && bit_depth != 2 && bit_depth != 4 {
        return Err(CafeError::UnsupportedFeature(format!(
            "pack_samples_row: bit_depth {} not supported",
            bit_depth
        )));
    }

    // Computes the expected packed buffer size
    let bits_total = (width as u64)
        .checked_mul(bit_depth as u64)
        .and_then(|result| result.checked_mul(bpp as u64))
        .ok_or_else(|| {
            CafeError::TruncatedFile("pack_samples_row: overflow in size calculation".into())
        })?;
    let packed_len = bits_total.div_ceil(8) as usize;
    let mut out = vec![0u8; packed_len];

    let _per_byte = 8 / bit_depth as usize;
    let mask = (1u8 << bit_depth) - 1;

    for i in 0..width {
        for ch in 0..bpp {
            let sample_idx = i * bpp + ch;
            if sample_idx >= samples.len() {
                return Err(CafeError::TruncatedFile(format!(
                    "pack_samples_row: sample_idx {} >= samples.len() {}",
                    sample_idx,
                    samples.len()
                )));
            }
            let value = samples[sample_idx];
            if value > mask {
                return Err(CafeError::UnsupportedFeature(format!(
                    "pack_samples_row: sample {} does not fit in {} bits (maximum {})",
                    value, bit_depth, mask
                )));
            }

            // Computes the bit-exact position of the sample in the packed output
            let bit_pos = (i * bpp + ch) * bit_depth as usize;
            let byte_idx = bit_pos / 8;
            let bit_offset = 8 - ((bit_pos % 8) + bit_depth as usize);

            if byte_idx >= out.len() {
                return Err(CafeError::TruncatedFile(format!(
                    "pack_samples_row: byte_idx {} >= out.len() {}",
                    byte_idx,
                    out.len()
                )));
            }
            out[byte_idx] |= (value & mask) << bit_offset;
        }
    }
    Ok(out)
}

/// Reverses `pack_samples_row`: recovers values from packed data.
/// Works for Grayscale and Grayscale+Alpha with bit depth 1, 2, 4.
///
/// # Parameters
/// - `packed`: data packed in bit_depth bits
/// - `bit_depth`: 1, 2, 4, or 8
/// - `width`: number of pixels
/// - `bpp`: bytes per pixel (channels)
///
/// # Safety
/// - Validates the packed buffer size vs. expected
/// - Returns Err if the buffer is too small
pub(crate) fn unpack_samples_row(
    packed: &[u8],
    bit_depth: u8,
    width: usize,
    bpp: usize,
) -> Result<Vec<u8>> {
    if bit_depth == 8 {
        let expected = width * bpp;
        if packed.len() < expected {
            return Err(CafeError::TruncatedFile(format!(
                "unpack_samples_row: buffer curto para bit_depth=8, esperado {} obtido {}",
                expected,
                packed.len()
            )));
        }
        return Ok(packed[..expected].to_vec());
    }

    if bit_depth != 1 && bit_depth != 2 && bit_depth != 4 {
        return Err(CafeError::UnsupportedFeature(format!(
            "unpack_samples_row: bit_depth {} not supported",
            bit_depth
        )));
    }

    // Validates buffer size
    let bits_total = (width as u64)
        .checked_mul(bit_depth as u64)
        .and_then(|result| result.checked_mul(bpp as u64))
        .ok_or_else(|| {
            CafeError::TruncatedFile(
                "unpack_samples_row: overflow in bits total calculation".into(),
            )
        })?;
    let expected_packed_len = bits_total.div_ceil(8) as usize;
    if packed.len() < expected_packed_len {
        return Err(CafeError::TruncatedFile(format!(
            "unpack_samples_row: buffer curto, esperado {} bytes obtido {}",
            expected_packed_len,
            packed.len()
        )));
    }

    let mask = (1u8 << bit_depth) - 1;
    let mut out = Vec::with_capacity(width * bpp);

    for i in 0..width {
        for ch in 0..bpp {
            let bit_pos = (i * bpp + ch) * bit_depth as usize;
            let byte_idx = bit_pos / 8;
            let bit_offset = 8 - ((bit_pos % 8) + bit_depth as usize);

            if byte_idx >= packed.len() {
                return Err(CafeError::TruncatedFile(format!(
                    "unpack_samples_row: byte_idx {} >= packed.len() {}",
                    byte_idx,
                    packed.len()
                )));
            }
            let value = (packed[byte_idx] >> bit_offset) & mask;
            out.push(value);
        }
    }
    Ok(out)
}

/// Reverses `pack_indices_row`: recovers values (1 byte each) from
/// a row packed in `bit_depth` bits.
///
/// # Safety
/// - Validates the packed buffer size vs. the expected width
/// - Returns Err if the buffer is too small
pub(crate) fn unpack_indices_row(packed: &[u8], bit_depth: u8, width: usize) -> Result<Vec<u8>> {
    if bit_depth == 8 {
        if packed.len() < width {
            return Err(CafeError::TruncatedFile(format!(
                "Buffer packed curto: esperado >= {} bytes, obtido {}",
                width,
                packed.len()
            )));
        }
        return Ok(packed[..width].to_vec());
    }

    if bit_depth != 1 && bit_depth != 2 && bit_depth != 4 {
        return Err(CafeError::UnsupportedFeature(format!(
            "unpack_indices_row: bit_depth {} not supported",
            bit_depth
        )));
    }

    // Validates buffer size
    let expected_packed_len = (width * bit_depth as usize).div_ceil(8);
    if packed.len() < expected_packed_len {
        return Err(CafeError::TruncatedFile(format!(
            "Buffer packed curto para width={}, bit_depth={}: esperado {} bytes, obtido {}",
            width,
            bit_depth,
            expected_packed_len,
            packed.len()
        )));
    }

    let per_byte = 8 / bit_depth as usize;
    let mask = (1u8 << bit_depth) - 1;
    let mut out = Vec::with_capacity(width);

    for i in 0..width {
        let byte_idx = i / per_byte;
        let slot = i % per_byte;
        let shift = 8 - bit_depth as usize * (slot + 1);
        out.push((packed[byte_idx] >> shift) & mask);
    }
    Ok(out)
}
