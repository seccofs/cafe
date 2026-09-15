//! HDR (float32) <-> CAFE pixel-buffer bridge.
//!
//! `png_io`'s scope is deliberately narrow (8-bit uint only); this module
//! is its float32 counterpart, covering `bit_depth = 32` /
//! `sample_format = SAMPLE_FORMAT_FLOAT` (spec section 4.1) - the same
//! representation `cafe_bench::measure::measure_hdr` already exercises
//! against `corpus/hdr/`'s `.exr` fixtures (see `AGENTS.md`'s HDR
//! benchmark wiring phase), now exposed through `cafe-encode`/`cafe-decode`
//! instead of only `cafe-bench`'s own harness.
//!
//! Scope: RGB/RGBA float32 only (`COLOR_TYPE_RGB`/`COLOR_TYPE_RGBA`) - the
//! `image` crate's own OpenEXR codec is RGB/RGBA-only to begin with (no
//! gray/gray+alpha EXR channel layout), so there is no narrower case to
//! additionally support here. Byte order is big-endian per spec section
//! 4.1's rule for any sample wider than 8 bits, matching
//! `cafe_bench::measure::measure_hdr`'s existing convention.
//!
//! Deliberately dispatches on `image::ColorType::Rgb32F`/`Rgba32F` (the
//! *decoded* color type) rather than sniffing a `.exr` file extension -
//! this bridge only cares about the in-memory sample shape, not which
//! container format produced it, mirroring `png_io`'s own
//! `has_alpha()`/`has_color()`-driven dispatch. In practice, `image` 0.25
//! only ever decodes into `Rgb32F`/`Rgba32F` for `.exr` sources (its other
//! decoders never produce float32 output), so the two bridges' input
//! never overlaps.

use cafe_format::constants::{COLOR_TYPE_RGB, COLOR_TYPE_RGBA, SAMPLE_FORMAT_FLOAT};
use image::{ColorType, DynamicImage, Rgb32FImage, Rgba32FImage};
use std::fmt;

use crate::png_io::CafePixels;

/// Errors converting between `image`'s float32 representation and CAFE's.
#[derive(Debug)]
pub enum HdrIoError {
    /// The decoded `image::DynamicImage` isn't one of the two float32
    /// color types this bridge understands.
    UnsupportedImageColorType(ColorType),
    /// The CAFE image's `bit_depth`/`sample_format` isn't representable as
    /// float32 by this bridge (only `bit_depth = 32` /
    /// `sample_format = SAMPLE_FORMAT_FLOAT` is).
    UnsupportedCafeFormat { bit_depth: u8, sample_format: u8 },
    /// `color_type` is not `COLOR_TYPE_RGB`/`COLOR_TYPE_RGBA` - the only
    /// two channel layouts `image`'s OpenEXR codec round-trips.
    UnsupportedCafeColorType(u8),
}

impl fmt::Display for HdrIoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HdrIoError::UnsupportedImageColorType(color_type) => {
                write!(
                    f,
                    "unsupported image color type {color_type:?} (hdr_io only accepts \
                     Rgb32F/Rgba32F decoded input)"
                )
            }
            HdrIoError::UnsupportedCafeFormat {
                bit_depth,
                sample_format,
            } => write!(
                f,
                "unsupported CAFE bit_depth={bit_depth}/sample_format={sample_format} \
                 (hdr_io only writes float32 RGB/RGBA)"
            ),
            HdrIoError::UnsupportedCafeColorType(color_type) => {
                write!(f, "unsupported CAFE color_type={color_type} for hdr_io")
            }
        }
    }
}

impl std::error::Error for HdrIoError {}

/// Converts a decoded float32 `image::DynamicImage` (`Rgb32F`/`Rgba32F` -
/// in practice, always an `.exr` source) into CAFE's raw-pixel shape.
/// Rejects any other decoded color type rather than lossily downconverting
/// it - callers should fall back to `png_io::dynamic_image_to_cafe_pixels`
/// for 8-bit sources instead.
pub fn dynamic_image_to_cafe_pixels_hdr(img: &DynamicImage) -> Result<CafePixels, HdrIoError> {
    let width = img.width();
    let height = img.height();

    let (color_type, samples): (u8, &[f32]) = match img {
        DynamicImage::ImageRgb32F(buf) => (COLOR_TYPE_RGB, buf.as_raw()),
        DynamicImage::ImageRgba32F(buf) => (COLOR_TYPE_RGBA, buf.as_raw()),
        other => return Err(HdrIoError::UnsupportedImageColorType(other.color())),
    };

    let mut raw_pixels = Vec::with_capacity(samples.len() * 4);
    for &sample in samples {
        raw_pixels.extend_from_slice(&sample.to_be_bytes());
    }

    Ok(CafePixels {
        width,
        height,
        bit_depth: 32,
        sample_format: SAMPLE_FORMAT_FLOAT,
        color_type,
        raw_pixels,
    })
}

/// Converts a decoded CAFE image's raw float32 pixels back into an
/// `image::DynamicImage`, ready to be written out via any encoder the
/// `image` crate supports for `Rgb32F`/`Rgba32F` (`.exr` in practice).
/// Only `bit_depth = 32`/`sample_format = SAMPLE_FORMAT_FLOAT` is accepted
/// (see module docs).
pub fn cafe_pixels_to_dynamic_image_hdr(
    width: u32,
    height: u32,
    bit_depth: u8,
    sample_format: u8,
    color_type: u8,
    raw_pixels: &[u8],
) -> Result<DynamicImage, HdrIoError> {
    if bit_depth != 32 || sample_format != SAMPLE_FORMAT_FLOAT {
        return Err(HdrIoError::UnsupportedCafeFormat {
            bit_depth,
            sample_format,
        });
    }

    let samples: Vec<f32> = raw_pixels
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| f32::from_be_bytes(*chunk))
        .collect();

    match color_type {
        COLOR_TYPE_RGB => {
            let buf = Rgb32FImage::from_raw(width, height, samples)
                .expect("raw_pixels length already validated by cafe_codec::decode_bytes");
            Ok(DynamicImage::ImageRgb32F(buf))
        }
        COLOR_TYPE_RGBA => {
            let buf = Rgba32FImage::from_raw(width, height, samples)
                .expect("raw_pixels length already validated by cafe_codec::decode_bytes");
            Ok(DynamicImage::ImageRgba32F(buf))
        }
        other => Err(HdrIoError::UnsupportedCafeColorType(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rgb32f_roundtrip_through_dynamic_image() {
        let raw: Vec<f32> = vec![
            0.0, 0.5, 1.0, //
            2.5, -1.0, 3.25, //
            0.1, 0.2, 0.3, //
            100.0, 200.0, 300.0,
        ];
        let img = Rgb32FImage::from_raw(2, 2, raw.clone()).unwrap();
        let dynimg = DynamicImage::ImageRgb32F(img);

        let pixels = dynamic_image_to_cafe_pixels_hdr(&dynimg).unwrap();
        assert_eq!(pixels.color_type, COLOR_TYPE_RGB);
        assert_eq!(pixels.bit_depth, 32);
        assert_eq!(pixels.sample_format, SAMPLE_FORMAT_FLOAT);
        assert_eq!(pixels.raw_pixels.len(), raw.len() * 4);

        let back = cafe_pixels_to_dynamic_image_hdr(
            pixels.width,
            pixels.height,
            pixels.bit_depth,
            pixels.sample_format,
            pixels.color_type,
            &pixels.raw_pixels,
        )
        .unwrap();
        assert_eq!(back.as_rgb32f().unwrap().as_raw(), &raw);
    }

    #[test]
    fn test_rgba32f_roundtrip_through_dynamic_image() {
        let raw: Vec<f32> = vec![
            0.0, 0.5, 1.0, 1.0, //
            2.5, -1.0, 3.25, 0.0, //
            0.1, 0.2, 0.3, 0.4, //
            100.0, 200.0, 300.0, 1.0,
        ];
        let img = Rgba32FImage::from_raw(2, 2, raw.clone()).unwrap();
        let dynimg = DynamicImage::ImageRgba32F(img);

        let pixels = dynamic_image_to_cafe_pixels_hdr(&dynimg).unwrap();
        assert_eq!(pixels.color_type, COLOR_TYPE_RGBA);

        let back = cafe_pixels_to_dynamic_image_hdr(
            pixels.width,
            pixels.height,
            pixels.bit_depth,
            pixels.sample_format,
            pixels.color_type,
            &pixels.raw_pixels,
        )
        .unwrap();
        assert_eq!(back.as_rgba32f().unwrap().as_raw(), &raw);
    }

    #[test]
    fn test_byte_order_is_big_endian() {
        let raw: Vec<f32> = vec![1.0, -2.5, 0.0];
        let img = Rgb32FImage::from_raw(1, 1, raw).unwrap();
        let dynimg = DynamicImage::ImageRgb32F(img);
        let pixels = dynamic_image_to_cafe_pixels_hdr(&dynimg).unwrap();

        let mut expected = Vec::new();
        expected.extend_from_slice(&1.0f32.to_be_bytes());
        expected.extend_from_slice(&(-2.5f32).to_be_bytes());
        expected.extend_from_slice(&0.0f32.to_be_bytes());
        assert_eq!(pixels.raw_pixels, expected);
    }

    #[test]
    fn test_unsupported_image_color_type_is_rejected() {
        let img = image::RgbaImage::from_raw(1, 1, vec![0, 0, 0, 255]).unwrap();
        let dynimg = DynamicImage::ImageRgba8(img);
        let result = dynamic_image_to_cafe_pixels_hdr(&dynimg);
        assert!(matches!(
            result,
            Err(HdrIoError::UnsupportedImageColorType(ColorType::Rgba8))
        ));
    }

    #[test]
    fn test_unsupported_cafe_bit_depth_is_rejected() {
        let result = cafe_pixels_to_dynamic_image_hdr(
            1,
            1,
            8,
            SAMPLE_FORMAT_FLOAT,
            COLOR_TYPE_RGB,
            &[0; 12],
        );
        assert!(matches!(
            result,
            Err(HdrIoError::UnsupportedCafeFormat { bit_depth: 8, .. })
        ));
    }

    #[test]
    fn test_unsupported_cafe_color_type_is_rejected() {
        let result = cafe_pixels_to_dynamic_image_hdr(
            1,
            1,
            32,
            SAMPLE_FORMAT_FLOAT,
            /* COLOR_TYPE_GRAY */ 0,
            &[0; 4],
        );
        assert!(matches!(
            result,
            Err(HdrIoError::UnsupportedCafeColorType(0))
        ));
    }
}
