//! HDR Tone-Mapping (Section 7 of the Spec)
//!
//! Converts HDR float data to SDR 8-bit sRGB using transfer functions
//! (PQ, HLG, sRGB) and color primaries (BT.709, BT.2020, DCI-P3).
//!
//! Enabled only if: sample_format == FLOAT && cHDR present

use crate::error::{CafeError, Result};
use crate::types::cHDR;

// Transfer Function EOTF (Electro-Optical Transfer Function)
// Converts encoded values [0, 1] to linear luminance

/// PQ EOTF (SMPTE ST.2084) — used in HDR10
/// ITU-R BT.2100 parameters
fn pq_eotf(x: f32, max_lum: f32) -> f32 {
    const M1: f32 = 0.1593;
    const M2: f32 = 78.8438;
    const C1: f32 = 0.8359;
    const C2: f32 = 18.8516;
    const C3: f32 = 18.6875;

    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return max_lum;
    }

    let x_m = x.powf(1.0 / M2);
    let numerator = (x_m - C1).max(0.0);
    let denominator = C2 - C3 * x_m;

    if denominator.abs() < 1e-10 {
        return max_lum; // Avoids division by zero
    }

    let lin = (numerator / denominator).powf(1.0 / M1);
    lin * max_lum
}

/// HLG EOTF (Hybrid Log-Gamma) — ITU-R BT.2100
/// Average luminance value ~100 nits
fn hlg_eotf(x: f32) -> f32 {
    const A: f32 = 0.17883;
    const B: f32 = 0.28466;
    const C: f32 = 0.55991;

    if x <= 0.5 {
        (x * x) / 3.0
    } else {
        ((A * (x - B)).exp() + C) / 12.0
    }
}

/// sRGB EOTF (IEC 61966-2-1)
/// Inverse sRGB companding
fn srgb_eotf(x: f32) -> f32 {
    if x <= 0.04045 {
        x / 12.92
    } else {
        ((x + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB Companding — linear luminance → sRGB encoded [0, 1]
fn srgb_companding(lin: f32) -> f32 {
    let lin = lin.clamp(0.0, 1.0);
    if lin <= 0.0031308 {
        lin * 12.92
    } else {
        1.055 * lin.powf(1.0 / 2.4) - 0.055
    }
}

// Color Primaries Matrices (hardcoded, pre-computed via CIE 1931)

struct ColorMatrix {
    m: [[f32; 3]; 3],
}

impl ColorMatrix {
    fn multiply(&self, rgb: &[f32; 3]) -> [f32; 3] {
        [
            self.m[0][0] * rgb[0] + self.m[0][1] * rgb[1] + self.m[0][2] * rgb[2],
            self.m[1][0] * rgb[0] + self.m[1][1] * rgb[1] + self.m[1][2] * rgb[2],
            self.m[2][0] * rgb[0] + self.m[2][1] * rgb[1] + self.m[2][2] * rgb[2],
        ]
    }
}

// Color primaries conversion matrices (CIE 1931, D65 white).
// Standard: RGB primaries → XYZ (matrices M_rgb_to_xyz).

fn matrix_bt709_to_xyz() -> ColorMatrix {
    // Rec.709 / sRGB primaries → XYZ
    ColorMatrix {
        m: [
            [0.412_456, 0.357_576, 0.180_438],
            [0.212_673, 0.715_152, 0.072_175],
            [0.019_334, 0.119_192, 0.950_304],
        ],
    }
}

fn matrix_bt2020_to_xyz() -> ColorMatrix {
    // Rec.2020 primaries → XYZ
    ColorMatrix {
        m: [
            [0.636_958, 0.144_617, 0.168_881],
            [0.262_700, 0.677_998, 0.059_302],
            [0.0, 0.028_073, 1.060_985],
        ],
    }
}

fn matrix_dcip3_to_xyz() -> ColorMatrix {
    // DCI-P3 (D65) primaries → XYZ
    ColorMatrix {
        m: [
            [0.486_571, 0.265_668, 0.198_217],
            [0.228_975, 0.691_739, 0.079_287],
            [0.0, 0.045_113, 1.043_944],
        ],
    }
}

fn matrix_xyz_to_bt709() -> ColorMatrix {
    // XYZ → Rec.709 / sRGB primaries (inverse of the matrix above)
    ColorMatrix {
        m: [
            [3.240_454, -1.537_139, -0.498_531],
            [-0.969_266, 1.876_011, 0.041_556],
            [0.055_643, -0.204_026, 1.057_225],
        ],
    }
}

fn matrix_xyz_to_bt2020() -> ColorMatrix {
    // XYZ → Rec.2020 primaries
    ColorMatrix {
        m: [
            [1.716_651, -0.355_671, -0.253_366],
            [-0.666_684, 1.616_481, 0.015_769],
            [0.017_640, -0.042_771, 0.942_103],
        ],
    }
}

fn matrix_xyz_to_dcip3() -> ColorMatrix {
    // XYZ → DCI-P3 (D65) primaries
    ColorMatrix {
        m: [
            [2.493_497, -0.931_384, -0.402_711],
            [-0.829_489, 1.762_664, 0.023_625],
            [0.035_846, -0.076_172, 0.956_885],
        ],
    }
}

/// RGB (in `primaries`) → XYZ matrix.
fn primaries_to_xyz(primaries: u8) -> ColorMatrix {
    match primaries {
        0 => matrix_bt709_to_xyz(),
        1 => matrix_bt2020_to_xyz(),
        _ => matrix_dcip3_to_xyz(),
    }
}

/// XYZ → RGB (in `primaries`) matrix.
fn xyz_to_primaries(primaries: u8) -> ColorMatrix {
    match primaries {
        0 => matrix_xyz_to_bt709(),
        1 => matrix_xyz_to_bt2020(),
        _ => matrix_xyz_to_dcip3(),
    }
}

/// Converts linear RGB between color primaries, going through XYZ.
/// Path: RGB_src → XYZ → RGB_dst. If src == dst, returns unchanged.
fn convert_primaries(rgb: &[f32; 3], src: u8, dst: u8) -> [f32; 3] {
    if src == dst {
        return *rgb;
    }
    let xyz = primaries_to_xyz(src).multiply(rgb);
    xyz_to_primaries(dst).multiply(&xyz)
}

/// Global tone-mapping operator (dynamic range compression).
/// Applies an S-shape/conic curve over the relative linear luminance [0, ∞)
/// and produces output in [0, 1].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToneMapOperator {
    /// `L_out = L / (1 + L)` — classic Reinhard operator (1996).
    /// Smoothly compresses highlights; slightly darkens midtones.
    #[allow(dead_code)] // Selectable in future use (CLI/decode configurable)
    Reinhard,
    /// ACES filmic curve (Narkowicz 2015) — `f(x) = x(2.51x + 0.03) /
    /// (x(2.43x + 0.59) + 0.14)`. Better preserves the perceived brightness of
    /// midtones with smooth roll-off in highlights. Default.
    Filmic,
}

impl ToneMapOperator {
    fn apply(&self, x: f32) -> f32 {
        let v = match self {
            ToneMapOperator::Reinhard => x / (1.0 + x),
            ToneMapOperator::Filmic => {
                let a = 2.51 * x + 0.03;
                let b = 2.43 * x + 0.59;
                (x * a) / (x * b + 0.14)
            }
        };
        // Filmic curve can exceed 1.0 for very large inputs; clamp
        v.clamp(0.0, 1.0)
    }
}

/// Main tone-mapping function
/// Converts HDR linear float → SDR 8-bit in target space
pub(crate) fn tonemap_hdr(
    rgb_linear: &[f32; 3],
    transfer_func: u8, // 0=linear, 1=PQ, 2=HLG, 3=sRGB
    color_src: u8,     // 0=BT.709, 1=BT.2020, 2=DCI-P3
    color_target: u8,  // same
    max_lum: f32,      // nits (10000 typical for PQ)
    operator: ToneMapOperator,
) -> Result<[f32; 3]> {
    // Validate enums
    if transfer_func > 3 {
        return Err(CafeError::UnsupportedFeature(format!(
            "Transfer function {} invalid",
            transfer_func
        )));
    }
    if color_src > 2 || color_target > 2 {
        return Err(CafeError::UnsupportedFeature(format!(
            "Invalid color primaries: src={}, target={}",
            color_src, color_target
        )));
    }

    // Clamp NaN/Inf: NaN does not survive comparisons, so handle explicitly
    let rgb_clamped = [
        if rgb_linear[0].is_finite() {
            rgb_linear[0].clamp(0.0, max_lum)
        } else {
            0.0
        },
        if rgb_linear[1].is_finite() {
            rgb_linear[1].clamp(0.0, max_lum)
        } else {
            0.0
        },
        if rgb_linear[2].is_finite() {
            rgb_linear[2].clamp(0.0, max_lum)
        } else {
            0.0
        },
    ];

    // If the transfer function is linear, it is already linear → no EOTF needed
    let rgb_linear_norm = if transfer_func != 0 {
        // Apply inverse transfer function (encoded → linear)
        let max_lum_safe = max_lum.max(1.0); // Protection against division by zero
        match transfer_func {
            1 => {
                // PQ: normalize to [0, 1] before EOTF
                let norm = [
                    rgb_clamped[0] / max_lum_safe,
                    rgb_clamped[1] / max_lum_safe,
                    rgb_clamped[2] / max_lum_safe,
                ];
                [
                    pq_eotf(norm[0], 1.0),
                    pq_eotf(norm[1], 1.0),
                    pq_eotf(norm[2], 1.0),
                ]
            }
            2 => {
                // HLG: values are already in [0, 1] or [0, 12]
                let hlg_scale = (max_lum_safe / 100.0).max(0.12); // Typical HLG scale
                let norm = [
                    (rgb_clamped[0] / hlg_scale).clamp(0.0, 1.0),
                    (rgb_clamped[1] / hlg_scale).clamp(0.0, 1.0),
                    (rgb_clamped[2] / hlg_scale).clamp(0.0, 1.0),
                ];
                [hlg_eotf(norm[0]), hlg_eotf(norm[1]), hlg_eotf(norm[2])]
            }
            3 => {
                // sRGB: values in [0, 1]
                let norm = [
                    rgb_clamped[0].clamp(0.0, 1.0),
                    rgb_clamped[1].clamp(0.0, 1.0),
                    rgb_clamped[2].clamp(0.0, 1.0),
                ];
                [srgb_eotf(norm[0]), srgb_eotf(norm[1]), srgb_eotf(norm[2])]
            }
            _ => unreachable!(),
        }
    } else {
        // Already linear
        [
            (rgb_clamped[0] / max_lum).clamp(0.0, 1.0),
            (rgb_clamped[1] / max_lum).clamp(0.0, 1.0),
            (rgb_clamped[2] / max_lum).clamp(0.0, 1.0),
        ]
    };

    // Color primaries conversion: RGB_linear(src) → XYZ → RGB_linear(target)
    let rgb_in_target = convert_primaries(&rgb_linear_norm, color_src, color_target);

    // Tone-mapping operator: compresses dynamic range → [0, 1]
    let mut out = [0f32; 3];
    for (i, v) in rgb_in_target.iter().enumerate() {
        // Relative input may have NaN/Inf if some protection failed — handle here
        let x = if v.is_finite() && *v >= 0.0 { *v } else { 0.0 };
        out[i] = operator.apply(x).clamp(0.0, 1.0);
    }

    Ok(out)
}

/// Apply tone-mapping to an entire image (float → SDR 8-bit)
pub(crate) fn apply_tone_mapping_to_image(
    pixels_float: &[u8], // RGBA float (each channel is f32, 4 channels = 16 bytes/pixel)
    width: u32,
    height: u32,
    chdr: &cHDR,
    target: u8, // 0=sRGB, 1=Rec709, 2=DCI-P3, 3=Linear
    operator: ToneMapOperator,
) -> Result<Vec<u8>> {
    let pixel_count = (width as u64)
        .checked_mul(height as u64)
        .ok_or_else(|| CafeError::TruncatedFile("overflow on width × height".into()))?
        as usize;

    // Validate size: each pixel is 16 bytes (4 floats × 4 bytes)
    let expected_bytes = pixel_count
        .checked_mul(16)
        .ok_or_else(|| CafeError::TruncatedFile("Overflow on pixel count × 16".into()))?;

    if pixels_float.len() != expected_bytes {
        return Err(CafeError::TruncatedFile(format!(
            "Tone-mapping: esperado {} bytes float, obtido {}",
            expected_bytes,
            pixels_float.len()
        )));
    }

    let mut result = Vec::with_capacity(pixel_count * 4); // RGBA 8-bit

    for i in 0..pixel_count {
        // Read 4 floats (RGBA) as big-endian
        let offset = i * 16;
        let r = f32::from_be_bytes([
            pixels_float[offset],
            pixels_float[offset + 1],
            pixels_float[offset + 2],
            pixels_float[offset + 3],
        ]);
        let g = f32::from_be_bytes([
            pixels_float[offset + 4],
            pixels_float[offset + 5],
            pixels_float[offset + 6],
            pixels_float[offset + 7],
        ]);
        let b = f32::from_be_bytes([
            pixels_float[offset + 8],
            pixels_float[offset + 9],
            pixels_float[offset + 10],
            pixels_float[offset + 11],
        ]);
        let a = f32::from_be_bytes([
            pixels_float[offset + 12],
            pixels_float[offset + 13],
            pixels_float[offset + 14],
            pixels_float[offset + 15],
        ]);

        // Tone-map RGB (A is left as-is)
        let rgb_linear = [r, g, b];
        let rgb_sdr = tonemap_hdr(
            &rgb_linear,
            chdr.transfer_function,
            chdr.color_primaries,
            target,
            chdr.max_luminance.max(1.0),
            operator,
        )?;

        // Apply sRGB companding (linear → [0, 1])
        let r_sdr = srgb_companding(rgb_sdr[0]);
        let g_sdr = srgb_companding(rgb_sdr[1]);
        let b_sdr = srgb_companding(rgb_sdr[2]);
        let a_sdr = a.clamp(0.0, 1.0);

        // Convert to 8-bit
        result.push((r_sdr * 255.0) as u8);
        result.push((g_sdr * 255.0) as u8);
        result.push((b_sdr * 255.0) as u8);
        result.push((a_sdr * 255.0) as u8);
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pq_eotf_linear_range() {
        // PQ with input [0, 1] should return [0, 10000]
        assert!(pq_eotf(0.0, 10000.0) < 1.0);
        let val_mid = pq_eotf(0.5, 10000.0);
        assert!(val_mid > 0.0 && val_mid < 10000.0);
        let val_max = pq_eotf(1.0, 10000.0);
        assert!(val_max >= 9999.0);
    }

    #[test]
    fn test_hlg_eotf_bounds() {
        let val0 = hlg_eotf(0.0);
        assert_eq!(val0, 0.0);
        let val05 = hlg_eotf(0.5);
        assert!(val05 > 0.0 && val05 < 0.5);
        let val1 = hlg_eotf(1.0);
        assert!(val1 > 0.1);
    }

    #[test]
    fn test_srgb_eotf_inverse_companding() {
        let encoded = 0.5;
        let linear = srgb_eotf(encoded);
        assert!(linear > 0.2 && linear < 0.3);
    }

    #[test]
    fn test_tonemap_nan_inf_clamped() {
        let rgb = [f32::NAN, f32::INFINITY, -1.0];
        let result = tonemap_hdr(&rgb, 0, 0, 0, 1.0, ToneMapOperator::Filmic).unwrap();
        assert!(result[0] >= 0.0 && result[0] <= 1.0);
        assert!(result[1] >= 0.0 && result[1] <= 1.0);
        assert!(result[2] >= 0.0 && result[2] <= 1.0);
    }

    #[test]
    fn test_tonemap_invalid_transfer_func() {
        let rgb = [0.5, 0.5, 0.5];
        assert!(tonemap_hdr(&rgb, 99, 0, 0, 1.0, ToneMapOperator::Filmic).is_err());
    }

    #[test]
    fn test_tonemap_invalid_primaries() {
        let rgb = [0.5, 0.5, 0.5];
        assert!(tonemap_hdr(&rgb, 0, 99, 0, 1.0, ToneMapOperator::Filmic).is_err());
    }

    #[test]
    fn test_tone_mapping_image_roundtrip() {
        // Create a 2×2 RGBA float image
        let mut pixels = Vec::new();
        for _ in 0..4 {
            pixels.extend_from_slice(&(0.5f32).to_be_bytes());
            pixels.extend_from_slice(&(0.5f32).to_be_bytes());
            pixels.extend_from_slice(&(0.5f32).to_be_bytes());
            pixels.extend_from_slice(&(1.0f32).to_be_bytes());
        }

        let chdr = cHDR {
            transfer_function: 1,
            color_primaries: 1,
            max_luminance: 10000.0,
            min_luminance: 0.001,
            max_cll: None,
            max_fall: None,
        };

        let result =
            apply_tone_mapping_to_image(&pixels, 2, 2, &chdr, 0, ToneMapOperator::Filmic).unwrap();
        assert_eq!(result.len(), 16);
        assert_eq!(result[3], 255);
        assert_eq!(result[7], 255);
        assert_eq!(result[11], 255);
        assert_eq!(result[15], 255);
    }

    #[test]
    fn test_tone_mapping_truncated_buffer() {
        let pixels = vec![0u8; 15];
        let chdr = cHDR {
            transfer_function: 0,
            color_primaries: 0,
            max_luminance: 100.0,
            min_luminance: 0.001,
            max_cll: None,
            max_fall: None,
        };
        assert!(
            apply_tone_mapping_to_image(&pixels, 1, 1, &chdr, 0, ToneMapOperator::Filmic).is_err()
        );
    }

    #[test]
    fn test_tonemap_zero_max_luminance() {
        let rgb = [0.5, 0.5, 0.5];
        let result = tonemap_hdr(&rgb, 1, 0, 0, 0.0, ToneMapOperator::Reinhard);
        assert!(result.is_ok());
    }

    #[test]
    fn test_tonemap_pq_above_range() {
        let rgb = [1.5, 2.0, -1.0];
        let result = tonemap_hdr(&rgb, 1, 0, 0, 10000.0, ToneMapOperator::Filmic);
        assert!(result.is_ok());
        let tonemapped = result.unwrap();
        assert!(tonemapped[0] >= 0.0 && tonemapped[0] <= 1.0);
    }

    #[test]
    fn test_tonemap_hlg_nan() {
        let rgb = [f32::NAN, 0.5, 0.5];
        let result = tonemap_hdr(&rgb, 2, 0, 0, 1.0, ToneMapOperator::Filmic);
        assert!(result.is_ok());
        let tonemapped = result.unwrap();
        assert!(!tonemapped[0].is_nan());
    }

    #[test]
    fn test_operator_bounds_and_monotonic() {
        // Both operators must map [0, ∞) → [0, 1] and be monotonic
        for op in [ToneMapOperator::Reinhard, ToneMapOperator::Filmic] {
            assert_eq!(op.apply(0.0), 0.0);
            assert!(op.apply(1.0) > 0.0 && op.apply(1.0) <= 1.0);
            assert!(op.apply(100.0) <= 1.0);
            let mut prev = 0.0;
            for i in 1..=100 {
                let v = op.apply(i as f32 / 100.0);
                assert!(v >= prev, "non-monotonic operator em {i}");
                prev = v;
            }
        }
    }

    #[test]
    fn test_operator_filmic_brighter_midtones() {
        // Filmic preserves more midtone brightness than pure Reinhard
        let f = ToneMapOperator::Filmic.apply(0.18);
        let r = ToneMapOperator::Reinhard.apply(0.18);
        assert!(f > r);
    }

    #[test]
    fn test_convert_primaries_same_identity() {
        let rgb = [0.2, 0.5, 0.8];
        assert_eq!(convert_primaries(&rgb, 0, 0), rgb);
        assert_eq!(convert_primaries(&rgb, 1, 1), rgb);
        assert_eq!(convert_primaries(&rgb, 2, 2), rgb);
    }

    #[test]
    fn test_convert_primaries_white_stays_white() {
        // White D65 (1,1,1) should remain ~(1,1,1) in any primaries
        for src in 0..=2u8 {
            for dst in 0..=2u8 {
                let out = convert_primaries(&[1.0, 1.0, 1.0], src, dst);
                for c in out {
                    assert!(
                        (c - 1.0).abs() < 0.05,
                        "white leak: src={src} dst={dst} -> {out:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn test_convert_primaries_roundtrip() {
        // BT.709 → BT.2020 → BT.709 should return to the original
        let rgb = [0.1, 0.6, 0.9];
        let to2020 = convert_primaries(&rgb, 0, 1);
        let back = convert_primaries(&to2020, 1, 0);
        for i in 0..3 {
            assert!(
                (back[i] - rgb[i]).abs() < 0.02,
                "roundtrip falhou: {rgb:?} -> {back:?}"
            );
        }
    }

    #[test]
    fn test_tonemap_primaries_conversion_applied() {
        // Without conversion (src==dst==0)
        let a = tonemap_hdr(&[0.5, 0.5, 0.5], 0, 0, 0, 1.0, ToneMapOperator::Filmic).unwrap();
        // With conversion 0→1 (BT.709 → BT.2020): values should differ
        let b = tonemap_hdr(&[0.5, 0.5, 0.5], 0, 0, 1, 1.0, ToneMapOperator::Filmic).unwrap();
        assert_ne!(a, b, "color primaries conversion had no effect");
    }

    #[test]
    fn test_tonemap_operator_changes_output() {
        let a = tonemap_hdr(&[0.5, 0.5, 0.5], 0, 0, 0, 1.0, ToneMapOperator::Reinhard).unwrap();
        let b = tonemap_hdr(&[0.5, 0.5, 0.5], 0, 0, 0, 1.0, ToneMapOperator::Filmic).unwrap();
        assert_ne!(a, b, "operators must produce different outputs");
    }
}
