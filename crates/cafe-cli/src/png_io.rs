//! PNG <-> CAFE pixel-buffer bridge (uint8/uint16, per spec section 4.1).
//!
//! `cafe-codec`'s `encode_bytes`/`decode_bytes` work in terms of raw,
//! row-major pixel bytes plus an explicit `(width, height, bit_depth,
//! sample_format, color_type)` tuple - they have no notion of PNG at all
//! (per `AGENTS.md`'s "boringly simple" decoder-first philosophy, image-file
//! I/O is deliberately kept out of `cafe-format`/`cafe-codec`). This module
//! is the one place in the workspace that bridges the two, for the
//! `cafe-encode`/`cafe-decode`/`cafe inspect` binaries.
//!
//! Scope: `sample_format = SAMPLE_FORMAT_UINT` at either `bit_depth = 8`
//! or `bit_depth = 16` (gray / gray+alpha / RGB / RGBA), matching every
//! uint combination spec section 4.1 allows. Which bit depth is used is
//! decided by the *decoded* `image::ColorType` (`L8`/`La8`/`Rgb8`/`Rgba8`
//! -> 8-bit, `L16`/`La16`/`Rgb16`/`Rgba16` -> 16-bit) rather than any CLI
//! flag - a PNG's own bit depth already tells `image` which variant to
//! decode into, so this bridge just preserves it instead of always
//! coercing down to 8-bit. 16-bit samples are big-endian on the wire, per
//! spec section 4.1's rule for any sample wider than 8 bits (the same
//! convention `hdr_io` already uses for float32). Float32/HDR sources
//! (`.exr`) are this module's one hard exclusion, not merely a narrowing:
//! `image` decodes those into `DynamicImage::ImageRgb32F`/`ImageRgba32F`,
//! which `has_color()`/`has_alpha()`-based dispatch below would otherwise
//! silently truncate to 8-bit uint, discarding the entire point of HDR
//! content - callers must route those through
//! `hdr_io::dynamic_image_to_cafe_pixels_hdr` instead (`cafe-encode` does
//! this by checking `img.color()` before calling either bridge).

use cafe_format::constants::{
    COLOR_TYPE_GRAY, COLOR_TYPE_GRAY_ALPHA, COLOR_TYPE_RGB, COLOR_TYPE_RGBA, SAMPLE_FORMAT_UINT,
};
use image::{ColorType, DynamicImage};
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
    /// The decoded `image::DynamicImage` is float32 (`Rgb32F`/`Rgba32F`,
    /// in practice always an `.exr` source) — refused rather than silently
    /// truncated to 8-bit uint, since that would discard the entire point
    /// of HDR content. Callers should route these through
    /// `hdr_io::dynamic_image_to_cafe_pixels_hdr` instead.
    Float32NotSupportedHere(image::ColorType),
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
                 (png_io only writes 8-bit or 16-bit uint PNGs)"
            ),
            PngIoError::UnsupportedCafeColorType(color_type) => {
                write!(f, "unsupported CAFE color_type={color_type}")
            }
            PngIoError::Float32NotSupportedHere(color_type) => write!(
                f,
                "image color type {color_type:?} is float32 (HDR) — use hdr_io instead of \
                 png_io for this input"
            ),
        }
    }
}

impl std::error::Error for PngIoError {}

/// Converts a `&[u16]` sample slice into big-endian bytes, per spec
/// section 4.1's rule for any sample wider than 8 bits — shared by every
/// 16-bit `color_type` branch below.
fn u16_samples_to_be_bytes(samples: &[u16]) -> Vec<u8> {
    let mut raw_pixels = Vec::with_capacity(samples.len() * 2);
    for &sample in samples {
        raw_pixels.extend_from_slice(&sample.to_be_bytes());
    }
    raw_pixels
}

/// Converts big-endian bytes back into a `Vec<u16>` — the inverse of
/// `u16_samples_to_be_bytes`, used when reconstructing an `image` 16-bit
/// buffer from a decoded CAFE image's raw pixels.
fn be_bytes_to_u16_samples(raw_pixels: &[u8]) -> Vec<u16> {
    raw_pixels
        .as_chunks::<2>()
        .0
        .iter()
        .map(|chunk| u16::from_be_bytes(*chunk))
        .collect()
}

/// Converts a decoded `image::DynamicImage` into CAFE's raw-pixel shape,
/// picking the narrowest CAFE `color_type` that preserves whether the
/// source has color and/or alpha (gray vs RGB, with vs without alpha) -
/// mirrors PNG's own color-type selection, just re-derived here since
/// `cafe-format`'s color types don't exactly match `image::ColorType`'s
/// full enumeration (no float variants in this bridge, see `hdr_io` for
/// those). `bit_depth` (8 or 16) is taken directly from the *decoded*
/// `image::ColorType` rather than always coercing down to 8-bit, so a
/// 16-bit source PNG round-trips at its original bit depth.
pub fn dynamic_image_to_cafe_pixels(img: &DynamicImage) -> Result<CafePixels, PngIoError> {
    let width = img.width();
    let height = img.height();
    let color = img.color();

    if matches!(color, ColorType::Rgb32F | ColorType::Rgba32F) {
        return Err(PngIoError::Float32NotSupportedHere(color));
    }

    let bit_depth: u8 = match color {
        ColorType::L16 | ColorType::La16 | ColorType::Rgb16 | ColorType::Rgba16 => 16,
        _ => 8,
    };

    let (color_type, raw_pixels) = if bit_depth == 16 {
        if color.has_alpha() {
            if color.has_color() {
                (
                    COLOR_TYPE_RGBA,
                    u16_samples_to_be_bytes(img.to_rgba16().as_raw()),
                )
            } else {
                (
                    COLOR_TYPE_GRAY_ALPHA,
                    u16_samples_to_be_bytes(img.to_luma_alpha16().as_raw()),
                )
            }
        } else if color.has_color() {
            (
                COLOR_TYPE_RGB,
                u16_samples_to_be_bytes(img.to_rgb16().as_raw()),
            )
        } else {
            (
                COLOR_TYPE_GRAY,
                u16_samples_to_be_bytes(img.to_luma16().as_raw()),
            )
        }
    } else if color.has_alpha() {
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
        bit_depth,
        sample_format: SAMPLE_FORMAT_UINT,
        color_type,
        raw_pixels,
    })
}

/// Converts a decoded CAFE image's raw pixels back into an
/// `image::DynamicImage`, ready to be written out via any encoder the
/// `image` crate supports (PNG in `cafe-decode`'s case). Only
/// `sample_format = SAMPLE_FORMAT_UINT` at `bit_depth = 8` or
/// `bit_depth = 16` is supported by this bridge (see module docs); 16-bit
/// samples are read as big-endian, the inverse of
/// `dynamic_image_to_cafe_pixels`' own encoding.
pub fn cafe_pixels_to_dynamic_image(
    width: u32,
    height: u32,
    bit_depth: u8,
    sample_format: u8,
    color_type: u8,
    raw_pixels: &[u8],
) -> Result<DynamicImage, PngIoError> {
    if sample_format != SAMPLE_FORMAT_UINT || (bit_depth != 8 && bit_depth != 16) {
        return Err(PngIoError::UnsupportedCafeFormat {
            bit_depth,
            sample_format,
        });
    }

    if bit_depth == 16 {
        let samples = be_bytes_to_u16_samples(raw_pixels);
        return match color_type {
            COLOR_TYPE_GRAY => {
                let buf =
                    image::ImageBuffer::<image::Luma<u16>, _>::from_raw(width, height, samples)
                        .expect("raw_pixels length already validated by cafe_codec::decode_bytes");
                Ok(DynamicImage::ImageLuma16(buf))
            }
            COLOR_TYPE_GRAY_ALPHA => {
                let buf =
                    image::ImageBuffer::<image::LumaA<u16>, _>::from_raw(width, height, samples)
                        .expect("raw_pixels length already validated by cafe_codec::decode_bytes");
                Ok(DynamicImage::ImageLumaA16(buf))
            }
            COLOR_TYPE_RGB => {
                let buf =
                    image::ImageBuffer::<image::Rgb<u16>, _>::from_raw(width, height, samples)
                        .expect("raw_pixels length already validated by cafe_codec::decode_bytes");
                Ok(DynamicImage::ImageRgb16(buf))
            }
            COLOR_TYPE_RGBA => {
                let buf =
                    image::ImageBuffer::<image::Rgba<u16>, _>::from_raw(width, height, samples)
                        .expect("raw_pixels length already validated by cafe_codec::decode_bytes");
                Ok(DynamicImage::ImageRgba16(buf))
            }
            other => Err(PngIoError::UnsupportedCafeColorType(other)),
        };
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
    fn test_16bit_gray_roundtrip_preserves_bit_depth() {
        // A 16-bit gray source now round-trips at its original bit depth
        // (big-endian on the wire), rather than being downconverted to
        // 8-bit — the behavior this bridge had before 16-bit CLI support.
        let raw16: Vec<u16> = vec![0x1234, 0x5678, 0x9ABC, 0xDEF0];
        let img = image::ImageBuffer::<image::Luma<u16>, _>::from_raw(2, 2, raw16.clone()).unwrap();
        let dynimg = DynamicImage::ImageLuma16(img);
        let pixels = dynamic_image_to_cafe_pixels(&dynimg).unwrap();
        assert_eq!(pixels.bit_depth, 16);
        assert_eq!(pixels.color_type, COLOR_TYPE_GRAY);
        assert_eq!(pixels.raw_pixels.len(), 8);
        let mut expected = Vec::new();
        for s in &raw16 {
            expected.extend_from_slice(&s.to_be_bytes());
        }
        assert_eq!(pixels.raw_pixels, expected);

        let back = cafe_pixels_to_dynamic_image(
            pixels.width,
            pixels.height,
            pixels.bit_depth,
            pixels.sample_format,
            pixels.color_type,
            &pixels.raw_pixels,
        )
        .unwrap();
        assert_eq!(back.to_luma16().into_raw(), raw16);
    }

    #[test]
    fn test_16bit_rgba_roundtrip_through_dynamic_image() {
        let raw16: Vec<u16> = vec![
            0x0001, 0x0203, 0x0405, 0xFFFF, //
            0x1111, 0x2222, 0x3333, 0x4444,
        ];
        let img = image::ImageBuffer::<image::Rgba<u16>, _>::from_raw(1, 2, raw16.clone()).unwrap();
        let dynimg = DynamicImage::ImageRgba16(img);
        let pixels = dynamic_image_to_cafe_pixels(&dynimg).unwrap();
        assert_eq!(pixels.bit_depth, 16);
        assert_eq!(pixels.color_type, COLOR_TYPE_RGBA);

        let back = cafe_pixels_to_dynamic_image(
            pixels.width,
            pixels.height,
            pixels.bit_depth,
            pixels.sample_format,
            pixels.color_type,
            &pixels.raw_pixels,
        )
        .unwrap();
        assert_eq!(back.to_rgba16().into_raw(), raw16);
    }

    #[test]
    fn test_16bit_byte_order_is_big_endian() {
        let raw16: Vec<u16> = vec![0x1234, 0xABCD];
        let img = image::ImageBuffer::<image::Luma<u16>, _>::from_raw(2, 1, raw16).unwrap();
        let dynimg = DynamicImage::ImageLuma16(img);
        let pixels = dynamic_image_to_cafe_pixels(&dynimg).unwrap();
        assert_eq!(pixels.raw_pixels, vec![0x12, 0x34, 0xAB, 0xCD]);
    }

    #[test]
    fn test_float32_source_is_rejected_not_downconverted() {
        let img = image::Rgb32FImage::from_raw(1, 1, vec![1.0, 2.0, 3.0]).unwrap();
        let dynimg = DynamicImage::ImageRgb32F(img);
        let result = dynamic_image_to_cafe_pixels(&dynimg);
        assert!(matches!(
            result,
            Err(PngIoError::Float32NotSupportedHere(
                image::ColorType::Rgb32F
            ))
        ));
    }

    #[test]
    fn test_unsupported_cafe_bit_depth_is_rejected() {
        // bit_depth = 32 with SAMPLE_FORMAT_UINT isn't a valid combination
        // per spec section 4.1 (uint is only 8 or 16; 32-bit is
        // float-only, handled by `hdr_io` instead) - `png_io` rejects it
        // rather than silently accepting it.
        let result =
            cafe_pixels_to_dynamic_image(1, 1, 32, SAMPLE_FORMAT_UINT, COLOR_TYPE_GRAY, &[0; 4]);
        assert!(matches!(
            result,
            Err(PngIoError::UnsupportedCafeFormat { bit_depth: 32, .. })
        ));
    }
}
