//! Interlacing functions (interlace): Adam7 and Even/Odd
//!
//! Implementation of two interlacing schemes:
//! - Adam7 (section 5): 7 progressive passes that cover the whole image
//! - Even/Odd (section 5): 2 passes for simple interlacing

use crate::constants::*;
use crate::error::{CafeError, Result};

/// Extracts a specific Adam7 pass from complete RGBA data.
///
/// Each pass covers only pixels not covered by earlier passes.
/// Section 5.1 of the spec.
pub(crate) fn extract_adam7_pass(raw: &[u8], width: u32, height: u32, pass: usize) -> Vec<u8> {
    if pass >= ADAM7_NUM_PASSES {
        return Vec::new();
    }

    // SECURITY: Computes pixel_count with overflow protection
    let pixel_count: u64 = match (width as u64).checked_mul(height as u64) {
        Some(count) => count,
        None => return Vec::new(), // Overflow detected, return empty
    };
    if pixel_count == 0 {
        return Vec::new();
    }

    // SECURITY: Limits the size of covered[] to usize::MAX
    if pixel_count > usize::MAX as u64 {
        return Vec::new(); // Impossible dimensions, return empty
    }
    let covered_size = pixel_count as usize;

    // Mark which pixels were already covered in earlier passes
    let mut covered = vec![false; covered_size];

    for &(x_step, y_step, x_offset, y_offset) in &ADAM7_PASSES[..pass] {
        let mut y = y_offset;
        while y < height {
            let mut x = x_offset;
            while x < width {
                // SECURITY: Computes the index using u64 to prevent overflow
                let idx_u64 = (y as u64).wrapping_mul(width as u64).wrapping_add(x as u64);
                let idx = idx_u64 as usize;
                if idx < covered.len() {
                    covered[idx] = true;
                }
                x += x_step;
            }
            y += y_step;
        }
    }

    // Extract only pixels new in this pass
    let (x_step, y_step, x_offset, y_offset) = ADAM7_PASSES[pass];
    let mut pass_data = Vec::new();

    let mut y = y_offset;
    while y < height {
        let mut x = x_offset;
        while x < width {
            // SECURITY: Computes the index using u64
            let idx_u64 = (y as u64).wrapping_mul(width as u64).wrapping_add(x as u64);
            let idx = idx_u64 as usize;
            let pixel_idx = idx.saturating_mul(BPP);
            if idx < covered.len() && !covered[idx] && pixel_idx + BPP <= raw.len() {
                pass_data.extend_from_slice(&raw[pixel_idx..pixel_idx + BPP]);
            }
            x += x_step;
        }
        y += y_step;
    }

    pass_data
}

/// Reconstructs the complete RGBA image from 7 Adam7 passes (progressive, without redundancy).
///
/// Each pass provides only NEW pixels (not covered by earlier passes).
/// This function fills the final image by inserting pixels in their correct positions,
/// respecting the progressive order of the passes.
///
/// # Security (CWE-190/CWE-400)
/// The dimensions come from the IHDR of a potentially untrusted file, and
/// `width × height` can overflow u32/usize. Before the fix this caused
/// a panic due to overflow in debug builds and `index out of bounds` in release
/// (a forged ~49-byte file crashed the decoder). Now:
/// - Every multiplication uses checked arithmetic (u64) with explicit rejection;
/// - The final buffer is only allocated if the data present in the passes is
///   exactly enough for the declared dimensions (spec section 12.3:
///   incremental reconstruction, without speculative pre-allocation);
/// - Internal indices computed in u64 to avoid intermediate overflow.
///
/// # Arguments
/// - `passes`: Array of 7 vectors (one per pass), containing only new pixels
/// - `width`: Image width in pixels
/// - `height`: Image height in pixels
///
/// # Returns
/// Complete RGBA vector (width × height × 4 bytes)
pub(crate) fn reconstruct_adam7(
    passes: &[Vec<u8>; ADAM7_NUM_PASSES],
    width: u32,
    height: u32,
) -> Result<Vec<u8>> {
    let total_pixels = (width as u64)
        .checked_mul(height as u64)
        .ok_or_else(|| CafeError::TruncatedFile("Adam7: overflow on width × height".into()))?;
    if total_pixels == 0 {
        return Err(CafeError::TruncatedFile(
            "Adam7: degenerate dimensions (0 pixels)".into(),
        ));
    }
    let expected_bytes = total_pixels
        .checked_mul(BPP as u64)
        .ok_or_else(|| CafeError::TruncatedFile("Adam7: overflow on total_pixels × BPP".into()))?;

    // SECURITY: only allocates the final buffer if the data actually present is
    // sufficient (and exact) to fill the declared dimensions. This prevents
    // both overflow and speculative allocation of gigabytes.
    let available: u64 = passes.iter().try_fold(0u64, |acc, p| {
        acc.checked_add(p.len() as u64).ok_or_else(|| {
            CafeError::TruncatedFile("Adam7: overflow when summing pass sizes".into())
        })
    })?;
    if available != expected_bytes {
        return Err(CafeError::TruncatedFile(format!(
            "Adam7: inconsistent data — passes sum {available} bytes, expected {expected_bytes} \
              (IHDR {width}x{height} with incomplete/excess IDAT)"
        )));
    }
    if expected_bytes > usize::MAX as u64 {
        return Err(CafeError::TruncatedFile(
            "Adam7: imagem grande demais para este processo".into(),
        ));
    }

    let total_pixels = total_pixels as usize;
    let mut result = vec![0u8; expected_bytes as usize];
    let mut filled = vec![false; total_pixels]; // Tracks which pixels have been filled

    for (pass_idx, pass_data) in passes.iter().enumerate() {
        let (x_step, y_step, x_offset, y_offset) = ADAM7_PASSES[pass_idx];
        let mut pixel_offset = 0;

        // Validation: computes the expected dimensions for this pass (v1.0)
        // This ensures adam7_pass_dimensions and adam7_pass_pixel_count are used
        let (_expected_width, _expected_height) = adam7_pass_dimensions(width, height, pass_idx);
        let _expected_pixels = adam7_pass_pixel_count(width, height, pass_idx);

        let mut y = y_offset;
        while y < height {
            let mut x = x_offset;
            while x < width {
                // SECURITY: index in u64 to avoid u32 overflow on
                // very wide images (guaranteed < total_pixels).
                let idx = ((y as u64) * (width as u64) + (x as u64)) as usize;
                let pixel_idx = idx * BPP;

                // Fill only if not yet filled
                if !filled[idx]
                    && pixel_offset + BPP <= pass_data.len()
                    && pixel_idx + BPP <= result.len()
                {
                    result[pixel_idx..pixel_idx + BPP]
                        .copy_from_slice(&pass_data[pixel_offset..pixel_offset + BPP]);
                    filled[idx] = true;
                    pixel_offset += BPP;
                }
                x += x_step;
            }
            y += y_step;
        }
    }

    Ok(result)
}

/// Computes the dimensions of the image considered in a specific Adam7 pass.
/// Useful for validation and debugging.
pub(crate) fn adam7_pass_dimensions(width: u32, height: u32, pass: usize) -> (u32, u32) {
    if pass >= ADAM7_NUM_PASSES {
        return (0, 0);
    }

    let (x_step, y_step, x_offset, y_offset) = ADAM7_PASSES[pass];

    // Number of pixels = ceil((width - offset) / step)
    let pass_width = if width > x_offset {
        (width - x_offset).div_ceil(x_step)
    } else {
        0
    };

    let pass_height = if height > y_offset {
        (height - y_offset).div_ceil(y_step)
    } else {
        0
    };

    (pass_width, pass_height)
}

/// Counts the total number of pixels in an Adam7 pass
pub(crate) fn adam7_pass_pixel_count(width: u32, height: u32, pass: usize) -> u32 {
    let (w, h) = adam7_pass_dimensions(width, height, pass);
    w * h
}

/// Applies Adam7 interlacing to RGBA image data.
/// Returns an array of 7 passes (each containing only new pixels, without redundancy).
pub(crate) fn apply_adam7_interlace(
    raw: &[u8],
    width: u32,
    height: u32,
) -> [Vec<u8>; ADAM7_NUM_PASSES] {
    let mut passes: [Vec<u8>; ADAM7_NUM_PASSES] = Default::default();
    for (i, out) in passes.iter_mut().enumerate() {
        *out = extract_adam7_pass(raw, width, height, i);
    }
    passes
}

/// Extracts an even/odd pass (Interlace=2)
/// Pass 0: even rows (y=0, 2, 4, ...)
/// Pass 1: odd rows (y=1, 3, 5, ...)
pub(crate) fn extract_even_odd_pass(raw: &[u8], width: u32, height: u32, pass: usize) -> Vec<u8> {
    if pass >= EVEN_ODD_NUM_PASSES {
        return Vec::new();
    }

    let mut pass_data = Vec::new();
    let mut y = pass as u32;
    let width_usize = width as usize;

    while y < height {
        let row_start = (y as usize) * width_usize * BPP;
        let row_end = row_start + width_usize * BPP;
        if row_end <= raw.len() {
            pass_data.extend_from_slice(&raw[row_start..row_end]);
        }
        y += 2; // Skips to the next row (even or odd)
    }

    pass_data
}

/// Reconstructs the complete RGBA image from 2 even/odd passes.
///
/// # Security (CWE-190/CWE-400)
/// Same protections as `reconstruct_adam7`: checked arithmetic on dimensions
/// and the final buffer is allocated only when the present data covers exactly
/// the declared size.
pub(crate) fn reconstruct_even_odd(
    passes: &[Vec<u8>; EVEN_ODD_NUM_PASSES],
    width: u32,
    height: u32,
) -> Result<Vec<u8>> {
    let total_pixels = (width as u64)
        .checked_mul(height as u64)
        .ok_or_else(|| CafeError::TruncatedFile("Even/odd: overflow on width × height".into()))?;
    if total_pixels == 0 {
        return Err(CafeError::TruncatedFile(
            "Even/odd: degenerate dimensions (0 pixels)".into(),
        ));
    }
    let expected_bytes = total_pixels.checked_mul(BPP as u64).ok_or_else(|| {
        CafeError::TruncatedFile("Even/odd: overflow on total_pixels × BPP".into())
    })?;

    let available: u64 = passes.iter().try_fold(0u64, |acc, p| {
        acc.checked_add(p.len() as u64).ok_or_else(|| {
            CafeError::TruncatedFile("Even/odd: overflow when summing pass sizes".into())
        })
    })?;
    if available != expected_bytes {
        return Err(CafeError::TruncatedFile(format!(
             "Even/odd: inconsistent data - passes sum {available} bytes, expected {expected_bytes} \
              (IHDR {width}x{height} with incomplete/excess IDAT)"
        )));
    }
    if expected_bytes > usize::MAX as u64 {
        return Err(CafeError::TruncatedFile(
            "Even/odd: image too large for this process".into(),
        ));
    }

    let mut result = vec![0u8; expected_bytes as usize];
    let width_usize = width as usize;

    for (pass_idx, pass_data) in passes.iter().enumerate() {
        let mut y = pass_idx as u32;
        let mut row_offset = 0;

        while y < height {
            let row_size = width_usize * BPP;
            let result_row_start = (y as usize) * width_usize * BPP;
            let result_row_end = result_row_start + row_size;

            if row_offset + row_size <= pass_data.len() && result_row_end <= result.len() {
                result[result_row_start..result_row_end]
                    .copy_from_slice(&pass_data[row_offset..row_offset + row_size]);
            }

            row_offset += row_size;
            y += 2;
        }
    }

    Ok(result)
}

/// Applies even/odd interlacing to RGBA data
pub(crate) fn apply_even_odd_interlace(
    raw: &[u8],
    width: u32,
    height: u32,
) -> [Vec<u8>; EVEN_ODD_NUM_PASSES] {
    let mut passes: [Vec<u8>; EVEN_ODD_NUM_PASSES] = Default::default();
    for (i, out) in passes.iter_mut().enumerate() {
        *out = extract_even_odd_pass(raw, width, height, i);
    }
    passes
}
