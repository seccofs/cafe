//! PNG <-> CAFE pixel-buffer bridge (v0.1 CLI scope: 8-bit only).
//!
//! `cafe-codec`'s `encode_bytes`/`decode_bytes` work in terms of raw,
//! row-major pixel bytes plus an explicit `(width, height, bit_depth,
//! sample_format, color_type)` tuple - they have no notion of PNG at all
//! (per `AGENTS.md`'s "boringly simple" decoder-first philosophy, image-file
//! I/O is deliberately kept out of `cafe-format`/`cafe-codec`). This module
//! is the one place in the workspace that bridges the two, for the
//! `cafe-encode`/`cafe-decode`/`cafe inspect` binaries.
//!
//! Scope: PNG only, 8 bits per sample only (gray / gray+alpha / RGB /
//! RGBA - `sample_format = SAMPLE_FORMAT_UINT`). An input PNG with a
//! different bit depth (16-bit) or color model (indexed, 32-bit float) is
//! coerced down to the nearest 8-bit CAFE color type by the `image` crate's
//! own conversion (`to_luma8`/`to_rgb8`/...), preserving whether the image
//! has color and/or alpha but not its original bit depth - 16-bit/float32
//! CLI I/O is deferred to a later pass (`cafe-codec` itself already
//! supports those bit depths, see its `encoder`/`decoder` tests; only this
//! bridge is narrower for now).

use cafe_format::constants::{
    COLOR_TYPE_GRAY, COLOR_TYPE_GRAY_ALPHA, COLOR_TYPE_RGB, COLOR_TYPE_RGBA, SAMPLE_FORMAT_UINT,
};
use image::DynamicImage;
use std::fmt;

/// One fully-described CAFE-ready pixel buffer, decoupled from any
/// particular container format.
#[derive(Debug, Clone)]
pub struct CafePixels {
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub sample_format: u8,
    pub color_type: u8,
    pub raw_pixels: Vec<u8>,
}

/// Errors converting between `image`'s in-memory representation and CAFE's.
#[derive(Debug)]
pub enum PngIoError {
    /// The CAFE image's `bit_depth`/`sample_format` isn't representable as
    /// an 8-bit PNG by this bridge yet.
    UnsupportedCafeFormat { bit_depth: u8, sample_format: u8 },
    /// `color_type` is not one of the four CAFE 0.1 color types (spec
    /// section 4.1) — should be unreachable for any `Ihdr` that already
    /// passed `Ihdr::validate` (e.g. every image `cafe_codec::decode_bytes`
    /// returns), kept as a defensive `Result` rather than a panic for
    /// direct callers of this bridge.
    UnsupportedCafeColorType(u8),
}

impl fmt::Display for PngIoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PngIoError::UnsupportedCafeFormat {
                bit_depth,
                sample_format,
            } => write!(
                f,
                "unsupported CAFE bit_depth={bit_depth}/sample_format={sample_format} \
                 (cafe-cli v0.1 only writes 8-bit uint PNGs)"
            ),
            PngIoError::UnsupportedCafeColorType(color_type) => {
                write!(f, "unsupported CAFE color_type={color_type}")
            }
        }
    }
}

impl std::error::Error for PngIoError {}

/// Converts a decoded `image::DynamicImage` into CAFE's raw-pixel shape,
/// picking the narrowest 8-bit CAFE `color_type` that preserves whether the
/// source has color and/or alpha (gray vs RGB, with vs without alpha) -
/// mirrors PNG's own color-type selection, just re-derived here since
/// `cafe-format`'s color types don't exactly match `image::ColorType`'s
/// full enumeration (no 16-bit/float variants in this bridge yet).
pub fn dynamic_image_to_cafe_pixels(img: &DynamicImage) -> Result<CafePixels, PngIoError> {
    let width = img.width();
    let height = img.height();
    let color = img.color();

    let (color_type, raw_pixels) = if color.has_alpha() {
        if color.has_color() {
            (COLOR_TYPE_RGBA, img.to_rgba8().into_raw())
        } else {
            (COLOR_TYPE_GRAY_ALPHA, img.to_luma_alpha8().into_raw())
        }
    } else if color.has_color() {
        (COLOR_TYPE_RGB, img.to_rgb8().into_raw())
    } else {
        (COLOR_TYPE_GRAY, img.to_luma8().into_raw())
    };

    Ok(CafePixels {
        width,
        height,
        bit_depth: 8,
        sample_format: SAMPLE_FORMAT_UINT,
        color_type,
        raw_pixels,
    })
}

/// Converts a decoded CAFE image's raw pixels back into an
/// `image::DynamicImage`, ready to be written out via any encoder the
/// `image` crate supports (PNG in `cafe-decode`'s case). Only
/// `bit_depth = 8`/`sample_format = SAMPLE_FORMAT_UINT` is supported by
/// this bridge (see module docs).
pub fn cafe_pixels_to_dynamic_image(
    width: u32,
    height: u32,
    bit_depth: u8,
    sample_format: u8,
    color_type: u8,
    raw_pixels: &[u8],
) -> Result<DynamicImage, PngIoError> {
    if bit_depth != 8 || sample_format != SAMPLE_FORMAT_UINT {
        return Err(PngIoError::UnsupportedCafeFormat {
            bit_depth,
            sample_format,
        });
    }

    match color_type {
        COLOR_TYPE_GRAY => {
            let buf = image::GrayImage::from_raw(width, height, raw_pixels.to_vec())
                .expect("raw_pixels length already validated by cafe_codec::decode_bytes");
            Ok(DynamicImage::ImageLuma8(buf))
        }
        COLOR_TYPE_GRAY_ALPHA => {
            let buf = image::GrayAlphaImage::from_raw(width, height, raw_pixels.to_vec())
                .expect("raw_pixels length already validated by cafe_codec::decode_bytes");
            Ok(DynamicImage::ImageLumaA8(buf))
        }
        COLOR_TYPE_RGB => {
            let buf = image::RgbImage::from_raw(width, height, raw_pixels.to_vec())
                .expect("raw_pixels length already validated by cafe_codec::decode_bytes");
            Ok(DynamicImage::ImageRgb8(buf))
        }
        COLOR_TYPE_RGBA => {
            let buf = image::RgbaImage::from_raw(width, height, raw_pixels.to_vec())
                .expect("raw_pixels length already validated by cafe_codec::decode_bytes");
            Ok(DynamicImage::ImageRgba8(buf))
        }
        other => Err(PngIoError::UnsupportedCafeColorType(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cafe_format::constants::{COLOR_TYPE_GRAY, COLOR_TYPE_RGBA};

    #[test]
    fn test_rgba_roundtrip_through_dynamic_image() {
        let raw = vec![
            10, 20, 30, 255, 40, 50, 60, 255, //
            70, 80, 90, 255, 100, 110, 120, 255,
        ];
        let img = image::RgbaImage::from_raw(2, 2, raw.clone()).unwrap();
        let dynimg = DynamicImage::ImageRgba8(img);
        let pixels = dynamic_image_to_cafe_pixels(&dynimg).unwrap();
        assert_eq!(pixels.color_type, COLOR_TYPE_RGBA);
        assert_eq!(pixels.bit_depth, 8);
        assert_eq!(pixels.raw_pixels, raw);

        let back = cafe_pixels_to_dynamic_image(
            pixels.width,
            pixels.height,
            pixels.bit_depth,
            pixels.sample_format,
            pixels.color_type,
            &pixels.raw_pixels,
        )
        .unwrap();
        assert_eq!(back.to_rgba8().into_raw(), raw);
    }

    #[test]
    fn test_gray_roundtrip_through_dynamic_image() {
        let raw = vec![10u8, 20, 30, 40];
        let img = image::GrayImage::from_raw(2, 2, raw.clone()).unwrap();
        let dynimg = DynamicImage::ImageLuma8(img);
        let pixels = dynamic_image_to_cafe_pixels(&dynimg).unwrap();
        assert_eq!(pixels.color_type, COLOR_TYPE_GRAY);
        assert_eq!(pixels.raw_pixels, raw);

        let back = cafe_pixels_to_dynamic_image(
            pixels.width,
            pixels.height,
            pixels.bit_depth,
            pixels.sample_format,
            pixels.color_type,
            &pixels.raw_pixels,
        )
        .unwrap();
        assert_eq!(back.to_luma8().into_raw(), raw);
    }

    #[test]
    fn test_16bit_source_is_downconverted_to_8bit() {
        // A 16-bit gray source should still produce a valid 8-bit CAFE
        // buffer (image's own to_luma8() conversion), not an error.
        let raw16 = vec![0x1234u16, 0x5678, 0x9ABC, 0xDEF0];
        let img = image::ImageBuffer::<image::Luma<u16>, _>::from_raw(2, 2, raw16).unwrap();
        let dynimg = DynamicImage::ImageLuma16(img);
        let pixels = dynamic_image_to_cafe_pixels(&dynimg).unwrap();
        assert_eq!(pixels.bit_depth, 8);
        assert_eq!(pixels.color_type, COLOR_TYPE_GRAY);
        assert_eq!(pixels.raw_pixels.len(), 4);
    }

    #[test]
    fn test_unsupported_cafe_bit_depth_is_rejected() {
        let result =
            cafe_pixels_to_dynamic_image(1, 1, 16, SAMPLE_FORMAT_UINT, COLOR_TYPE_GRAY, &[0, 0]);
        assert!(matches!(
            result,
            Err(PngIoError::UnsupportedCafeFormat { bit_depth: 16, .. })
        ));
    }
}
