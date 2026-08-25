//! Benchmark image generation utilities for SIMD testing.
//!
//! Provides reproducible standard test images for consistent benchmarking
//! across all SIMD optimization phases.

use std::fmt;

/// Standard benchmark image specification
#[derive(Clone, Debug)]
pub struct BenchmarkImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>, // RGBA format: 4 bytes per pixel
}

impl fmt::Display for BenchmarkImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mb = (self.pixels.len() as f64) / (1024.0 * 1024.0);
        write!(
            f,
            "BenchmarkImage({}×{}, {:.1} MB, {} pixels)",
            self.width,
            self.height,
            mb,
            self.width * self.height
        )
    }
}

impl BenchmarkImage {
    /// Creates a standard 1920×1080 RGBA benchmark image with deterministic random data.
    ///
    /// # Seed
    /// Uses a fixed seed (0xDEADBEEF) for reproducibility across runs.
    ///
    /// # Returns
    /// A BenchmarkImage ready for encoding/decoding tests and benchmarks.
    pub fn standard_1920x1080() -> Self {
        Self::with_dimensions(1920, 1080, 0xDEADBEEF)
    }

    /// Creates a benchmark image with specified dimensions and seed.
    ///
    /// # Arguments
    /// - `width`: Image width in pixels
    /// - `height`: Image height in pixels
    /// - `seed`: Random seed for pixel data generation
    pub fn with_dimensions(width: u32, height: u32, seed: u64) -> Self {
        let num_pixels = (width as usize) * (height as usize);
        let mut pixels = vec![0u8; num_pixels * 4]; // RGBA

        // Generate deterministic random pixels using simple LCG
        // This gives variety without requiring external crates
        let mut state = seed;
        for pixel_data in pixels.chunks_mut(4) {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            pixel_data[0] = state as u8; // R
            pixel_data[1] = (state >> 8) as u8; // G
            pixel_data[2] = (state >> 16) as u8; // B
            pixel_data[3] = (state >> 24) as u8; // A
        }

        Self {
            width,
            height,
            pixels,
        }
    }

    /// Creates a checkerboard pattern image (useful for compression testing).
    pub fn checkerboard_pattern(width: u32, height: u32, square_size: u32) -> Self {
        let mut image = Self::with_dimensions(width, height, 0);

        for y in 0..height {
            for x in 0..width {
                let idx = ((y as usize) * (width as usize) + (x as usize)) * 4;
                let is_white = ((x / square_size) + (y / square_size)).is_multiple_of(2);

                if is_white {
                    image.pixels[idx] = 255;
                    image.pixels[idx + 1] = 255;
                    image.pixels[idx + 2] = 255;
                    image.pixels[idx + 3] = 255;
                } else {
                    image.pixels[idx] = 0;
                    image.pixels[idx + 1] = 0;
                    image.pixels[idx + 2] = 0;
                    image.pixels[idx + 3] = 255;
                }
            }
        }

        image
    }

    /// Creates a gradient pattern image.
    pub fn gradient_pattern(width: u32, height: u32) -> Self {
        let mut image = Self::with_dimensions(width, height, 0);

        for y in 0..height {
            for x in 0..width {
                let idx = ((y as usize) * (width as usize) + (x as usize)) * 4;
                let r = (x as f32 / width as f32 * 255.0) as u8;
                let g = (y as f32 / height as f32 * 255.0) as u8;
                let b = 128;
                let a = 255;

                image.pixels[idx] = r;
                image.pixels[idx + 1] = g;
                image.pixels[idx + 2] = b;
                image.pixels[idx + 3] = a;
            }
        }

        image
    }

    /// Returns image size in bytes.
    pub const fn size_bytes(&self) -> usize {
        self.pixels.len()
    }

    /// Returns number of pixels.
    pub const fn num_pixels(&self) -> u64 {
        (self.width as u64) * (self.height as u64)
    }

    /// Returns dimensions as string.
    pub fn dimensions_str(&self) -> String {
        format!("{}×{}", self.width, self.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_standard_image_dimensions() {
        let img = BenchmarkImage::standard_1920x1080();
        assert_eq!(img.width, 1920);
        assert_eq!(img.height, 1080);
        assert_eq!(img.num_pixels(), 1920 * 1080);
        assert_eq!(img.size_bytes(), 1920 * 1080 * 4);
    }

    #[test]
    fn test_deterministic_generation() {
        let img1 = BenchmarkImage::standard_1920x1080();
        let img2 = BenchmarkImage::standard_1920x1080();
        assert_eq!(img1.pixels, img2.pixels, "Images should be identical");
    }

    #[test]
    fn test_custom_dimensions() {
        let img = BenchmarkImage::with_dimensions(256, 256, 12345);
        assert_eq!(img.width, 256);
        assert_eq!(img.height, 256);
        assert_eq!(img.num_pixels(), 256 * 256);
    }

    #[test]
    fn test_checkerboard_pattern() {
        let img = BenchmarkImage::checkerboard_pattern(64, 64, 8);
        assert_eq!(img.width, 64);
        assert_eq!(img.height, 64);
        // Verify some pixels are white (255) and some are black (0)
        assert!(img.pixels.contains(&255));
        assert!(img.pixels.contains(&0));
    }
}
